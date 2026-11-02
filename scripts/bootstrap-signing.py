#!/usr/bin/env python3
"""Create the shared signing bundle once and upload it, encrypted, to object storage.

What it makes
  team/certs.tar.enc      certs/distribution.p12  (Apple Distribution — signs the apps, all platforms)
                          certs/installer.p12     (Mac Installer Distribution — signs the macOS .pkg)
                          certs/AppleWWDRCAG*.cer (the Apple intermediate each leaf chains to)
  <slug>/profiles.tar.enc profiles/<name>.mobileprovision | .provisionprofile — one per --profile,
                          or by default "<App> iOS App Store", "<App> macOS App Store", "<App> tvOS App Store"

CI fetches both with a read-only key (see .github/workflows/testflight.yml); fastlane imports them
(fastlane/Fastfile, prepare_signing). Private keys exist in plaintext only inside a temp dir that is
overwritten (best effort — APFS is copy-on-write) and removed before exit; they are never written
anywhere else and never into the repo.

Order of operations is defensive: everything that can refuse does so BEFORE the first write to Apple,
and the certificates are encrypted and uploaded the moment they exist — before profiles, before
anything else that could fail — so a later failure can never strand freshly created certificates
whose keys only this run ever held. If that first upload fails, the ENCRYPTED bundle is saved outside
the temp dir and its path printed; upload it by hand rather than creating new certificates.

Certificates are account-wide, so they are created ONCE per team. For a second app on the same team run
with --reuse-certs DIR (DIR holds the decrypted certs/ from a previous run) and only profiles are made.

Which profiles: by default one per platform (iOS, macOS, tvOS) for APP_BUNDLE_ID, named
"<APP_NAME> <platform> App Store". An app with extra signed targets (an extension, a watch app) or
fewer platforms lists its profiles explicitly instead, one --profile per signed bundle id:
  --profile iOS:com.example.app:"App App Store" --profile iOS:com.example.app.widgets:"App widgets App Store"
The names must match what fastlane/Fastfile spells in PROFILE_NAMES.

Environment
  ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_P8   App Store Connect API key (ADMIN role) and path to its .p8
  APP_BUNDLE_ID, APP_NAME, APP_SLUG         e.g. com.example.bud, Bud, bud
  AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY  the READ-WRITE bucket key (S3-compatible)
  B2_SIGNING_ENDPOINT, B2_SIGNING_BUCKET    e.g. s3.<region>.<host>, my-signing-bucket
  SIGNING_PASSPHRASE                        encrypts the .tar.enc blobs AND the .p12 files

Needs: python3, openssl, boto3 (python3 -m pip install boto3).
"""
import argparse, base64, json, os, re, shutil, subprocess, sys, tempfile, time, urllib.request, urllib.error

API = "https://api.appstoreconnect.apple.com"
# S3-compatible stores have rejected boto3's newer default checksum headers; send them only when an
# operation requires them. Must be set before boto3 is imported (it is imported lazily below).
os.environ.setdefault("AWS_REQUEST_CHECKSUM_CALCULATION", "when_required")
os.environ.setdefault("AWS_RESPONSE_CHECKSUM_VALIDATION", "when_required")
CERT_TYPES = [("distribution", "DISTRIBUTION"), ("installer", "MAC_INSTALLER_DISTRIBUTION")]
PROFILE_KINDS = {"iOS":   ("IOS_APP_STORE",  "mobileprovision"),   # also signs watchOS targets
                 "macOS": ("MAC_APP_STORE",  "provisionprofile"),
                 "tvOS":  ("TVOS_APP_STORE", "mobileprovision")}


def env(name):
    v = os.environ.get(name, "")
    if not v:
        sys.exit(f"missing environment variable {name}")
    return v


def sh(*cmd, input=None):
    return subprocess.run(cmd, input=input, capture_output=True, check=True).stdout


# ---- App Store Connect API (ES256 JWT via openssl; no extra dependencies) -------------------------
def b64url(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=")


def jwt(key_id, issuer, p8):
    h = b64url(json.dumps({"alg": "ES256", "kid": key_id, "typ": "JWT"}).encode())
    p = b64url(json.dumps({"iss": issuer, "iat": int(time.time()), "exp": int(time.time()) + 600,
                           "aud": "appstoreconnect-v1"}).encode())
    signing_input = h + b"." + p
    der = sh("openssl", "dgst", "-sha256", "-sign", p8, input=signing_input)
    i = 2 + ((der[1] & 0x7F) if der[1] & 0x80 else 0)

    def read_int(o):
        n = der[o + 1]
        return der[o + 2:o + 2 + n].lstrip(b"\x00"), o + 2 + n
    r, i = read_int(i)
    s, _ = read_int(i)
    return (signing_input + b"." + b64url(r.rjust(32, b"\x00") + s.rjust(32, b"\x00"))).decode()


class ASC:
    def __init__(self, key_id, issuer, p8):
        self.args = (key_id, issuer, p8)

    def call(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(API + path, data=data, method=method, headers={
            "Authorization": "Bearer " + jwt(*self.args), "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req) as r:
                raw = r.read()
                return r.status, (json.loads(raw) if raw else {})
        except urllib.error.HTTPError as e:
            raw = e.read().decode(errors="replace")
            try:
                return e.code, json.loads(raw)
            except ValueError:
                return e.code, {"raw": raw[:400]}

    def must(self, method, path, body=None, ok=(200, 201, 204)):
        st, r = self.call(method, path, body)
        if st not in ok:
            sys.exit(f"{method} {path} -> HTTP {st}\n{json.dumps(r, indent=1)[:1200]}")
        return r


# ---- certificates ----------------------------------------------------------------------------------
def wwdr_intermediate_for(cert_pem, certs_dir):
    """Download the Apple WWDR intermediate that issued this leaf (G3, G4, …) into certs_dir."""
    issuer = sh("openssl", "x509", "-noout", "-issuer", "-in", cert_pem).decode()
    m = re.search(r"OU\s*=\s*(G\d+)", issuer)
    gen = m.group(1) if m else "G3"
    out = os.path.join(certs_dir, f"AppleWWDRCA{gen}.cer")
    if not os.path.exists(out):
        urllib.request.urlretrieve(f"https://www.apple.com/certificateauthority/AppleWWDRCA{gen}.cer", out)
    return out


def refuse_if_certs_exist(asc, force):
    """Called BEFORE any write: a run must not create a second certificate of a type that exists."""
    for _, ctype in CERT_TYPES:
        existing = asc.must("GET", f"/v1/certificates?filter[certificateType]={ctype}&fields[certificates]=name").get("data", [])
        if existing and not force:
            names = ", ".join(f"{c['id']} ({c['attributes']['name']})" for c in existing)
            sys.exit(f"A {ctype} certificate already exists: {names}.\n"
                     f"Its private key lives only where it was created. Either run with --reuse-certs DIR "
                     f"(the decrypted certs/ from the run that made it) or --force-new-certs to add another "
                     f"(Apple caps these; revoke the old one afterwards if it is unused).")


def create_certs(asc, work):
    certs_dir = os.path.join(work, "certs")
    os.makedirs(certs_dir, exist_ok=True)
    made = {}
    for label, ctype in CERT_TYPES:
        key = os.path.join(work, f"{label}.key")
        csr = os.path.join(work, f"{label}.csr")
        sh("openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", csr,
           "-subj", "/CN=shared-ci-signing/O=ci")
        csr_pem = open(csr).read()
        r = asc.must("POST", "/v1/certificates", {"data": {"type": "certificates", "attributes": {
            "certificateType": ctype, "csrContent": csr_pem}}})
        cid = r["data"]["id"]
        der = base64.b64decode(r["data"]["attributes"]["certificateContent"])
        cer = os.path.join(work, f"{label}.cer")
        pem = os.path.join(work, f"{label}.pem")
        open(cer, "wb").write(der)
        sh("openssl", "x509", "-inform", "DER", "-in", cer, "-out", pem)
        cn = sh("openssl", "x509", "-noout", "-subject", "-in", pem).decode().strip()
        wwdr = wwdr_intermediate_for(pem, certs_dir)
        p12 = os.path.join(certs_dir, f"{label}.p12")
        # SHA1/3DES PKCS#12 algorithms: every macOS `security import` accepts them, where OpenSSL 3's
        # AES/PBKDF2 defaults are sometimes refused. Strength comes from the AES-256 blob around it.
        sh("openssl", "pkcs12", "-export", "-inkey", key, "-in", pem, "-name", f"{label} (shared CI)",
           "-keypbe", "PBE-SHA1-3DES", "-certpbe", "PBE-SHA1-3DES", "-macalg", "sha1",
           "-out", p12, "-passout", "env:SIGNING_PASSPHRASE")
        made[label] = {"id": cid, "type": ctype, "subject": cn, "p12": p12, "wwdr": os.path.basename(wwdr)}
        print(f"  certificate {label:<12} {cid}  {cn}")
    return certs_dir, made


def dist_cert_id(asc, reuse_dir):
    """When reusing certs, find the DISTRIBUTION certificate id that matches the reused .p12."""
    p12 = os.path.join(reuse_dir, "distribution.p12")
    pem = sh("openssl", "pkcs12", "-in", p12, "-nokeys", "-clcerts", "-passin", "env:SIGNING_PASSPHRASE")
    serial_local = sh("openssl", "x509", "-noout", "-serial", input=pem).decode().split("=")[1].strip().lstrip("0").lower()
    for c in asc.must("GET", "/v1/certificates?filter[certificateType]=DISTRIBUTION&fields[certificates]=serialNumber,name").get("data", []):
        if c["attributes"]["serialNumber"].lstrip("0").lower() == serial_local:
            return c["id"]
    sys.exit("the reused distribution.p12 does not match any DISTRIBUTION certificate on the account")


# ---- profiles --------------------------------------------------------------------------------------
def resolve_bundle_id(asc, bundle_id):
    """Called BEFORE any write: the bundle id record the profiles will hang off."""
    bids = asc.must("GET", f"/v1/bundleIds?filter[identifier]={bundle_id}&fields[bundleIds]=identifier,platform").get("data", [])
    bids = [b for b in bids if b["attributes"]["identifier"] == bundle_id]
    if not bids:
        sys.exit(f"no bundle id record for {bundle_id}")
    print(f"  bundle id {bundle_id} -> {bids[0]['id']} ({bids[0]['attributes']['platform']})")
    return bids[0]["id"]


def profile_specs(args, app_name, bundle_id):
    """[(kind, bundle_id, name)] — from --profile, or the three-platform default for APP_BUNDLE_ID."""
    if not args.profile:
        return [(k, bundle_id, f"{app_name} {k} App Store") for k in PROFILE_KINDS]
    specs = []
    for raw in args.profile:
        parts = raw.split(":", 2)
        if len(parts) != 3 or parts[0] not in PROFILE_KINDS or not parts[1] or not parts[2]:
            sys.exit(f"--profile wants KIND:BUNDLE_ID:NAME with KIND one of {', '.join(PROFILE_KINDS)}: {raw!r}")
        specs.append(tuple(parts))
    if len({s[2] for s in specs}) != len(specs):
        sys.exit("--profile names must be unique")
    return specs


def create_profiles(asc, work, specs, cert_id):
    """specs: [(kind, bundle_id_record_id, name)] — same-name profiles are replaced."""
    pdir = os.path.join(work, "profiles")
    os.makedirs(pdir, exist_ok=True)
    for kind, bid, name in specs:
        ptype, ext = PROFILE_KINDS[kind]
        for old in asc.must("GET", f"/v1/profiles?filter[name]={urllib.request.quote(name)}&fields[profiles]=name").get("data", []):
            if old["attributes"]["name"] == name:
                asc.must("DELETE", f"/v1/profiles/{old['id']}")
                print(f"  replaced stale profile {old['id']} ({name})")
        r = asc.must("POST", "/v1/profiles", {"data": {"type": "profiles",
            "attributes": {"name": name, "profileType": ptype},
            "relationships": {"bundleId": {"data": {"type": "bundleIds", "id": bid}},
                              "certificates": {"data": [{"type": "certificates", "id": cert_id}]}}}})
        content = base64.b64decode(r["data"]["attributes"]["profileContent"])
        path = os.path.join(pdir, f"{name}.{ext}")
        open(path, "wb").write(content)
        print(f"  profile {kind:<6} {r['data']['id']}  {name}")
    return pdir


# ---- bundle + upload -------------------------------------------------------------------------------
def encrypt_dir(work, subdir):
    tar = os.path.join(work, f"{subdir}.tar")
    enc = tar + ".enc"
    # COPYFILE_DISABLE keeps macOS tar from adding ._* AppleDouble entries to the archive.
    subprocess.run(["tar", "-C", work, "-cf", tar, subdir], check=True, capture_output=True,
                   env={**os.environ, "COPYFILE_DISABLE": "1"})
    sh("openssl", "enc", "-aes-256-cbc", "-pbkdf2", "-iter", "200000", "-salt",
       "-in", tar, "-out", enc, "-pass", "env:SIGNING_PASSPHRASE")
    os.remove(tar)
    return enc


def upload(enc, key, endpoint, bucket):
    import boto3  # imported late so --help works without it
    region = endpoint.split(".")[1] if endpoint.startswith("s3.") else None
    s3 = boto3.client("s3", endpoint_url=f"https://{endpoint}", region_name=region)
    s3.upload_file(enc, bucket, key)
    size = s3.head_object(Bucket=bucket, Key=key)["ContentLength"]
    print(f"  uploaded s3://{bucket}/{key}  ({size} bytes)")


def rescue_certs(work, why):
    """Something failed while the certificate keys live only in this run: encrypt whatever exists
    (finished .p12s, plus any raw key/cert not yet exported) into ~/signing-rescue-<ts>/ and stop.
    Nothing leaves the work dir in plaintext."""
    rescue = os.path.join(os.path.expanduser("~"), f"signing-rescue-{int(time.time())}")
    os.makedirs(rescue, exist_ok=True)
    saved = []
    certs = os.path.join(work, "certs")
    if os.path.isdir(certs) and os.listdir(certs):
        shutil.copy2(encrypt_dir(work, "certs"), os.path.join(rescue, "certs.tar.enc"))
        saved.append("certs.tar.enc (import-ready .p12 + WWDR)")
    raw = [f for f in os.listdir(work) if f.endswith((".key", ".pem", ".cer"))]
    if raw:
        os.makedirs(os.path.join(work, "raw"), exist_ok=True)
        for f in raw:
            shutil.move(os.path.join(work, f), os.path.join(work, "raw", f))
        shutil.copy2(encrypt_dir(work, "raw"), os.path.join(rescue, "raw.tar.enc"))
        saved.append("raw.tar.enc (unexported key/cert pairs)")
    sys.exit(f"{why}\nThe ENCRYPTED material was saved to {rescue}/ — " + "; ".join(saved) + ".\n"
             f"Finish by hand with the same passphrase (upload team/certs.tar.enc, then re-run with "
             f"--reuse-certs). Do NOT create new certificates: Apple caps them and the keys exist "
             f"only in that directory.")


def shred(path):
    """Overwrite then delete every file under path (plaintext keys lived here)."""
    for root, _, files in os.walk(path):
        for f in files:
            p = os.path.join(root, f)
            try:
                n = os.path.getsize(p)
                with open(p, "r+b") as fh:
                    fh.write(os.urandom(n))
            except OSError:
                pass
    shutil.rmtree(path, ignore_errors=True)


def preflight(asc, specs, endpoint, bucket):
    """--check: prove every prerequisite (auth, bundle ids, certificate state, bucket) and create nothing."""
    import boto3
    ok = True
    print("preflight:")
    print("  openssl:", sh("openssl", "version").decode().strip())
    asc.must("GET", "/v1/certificates?limit=1")
    print("  ASC auth OK")
    for kind, bundle_id, name in specs:
        bids = [b for b in asc.must("GET", f"/v1/bundleIds?filter[identifier]={bundle_id}&fields[bundleIds]=identifier,platform").get("data", [])
                if b["attributes"]["identifier"] == bundle_id]
        print(f"  profile {kind:<6} {name!r} for {bundle_id}: " + (f"{bids[0]['id']} ({bids[0]['attributes']['platform']})" if bids else "bundle id NOT FOUND"))
        ok = ok and bool(bids)
    for _, ctype in CERT_TYPES:
        ex = asc.must("GET", f"/v1/certificates?filter[certificateType]={ctype}&fields[certificates]=name").get("data", [])
        print(f"  existing {ctype:<26} {len(ex)}" + ("  (a run will refuse without --reuse-certs / --force-new-certs)" if ex else "  (none — a run will create one)"))
    region = endpoint.split(".")[1] if endpoint.startswith("s3.") else None
    s3 = boto3.client("s3", endpoint_url=f"https://{endpoint}", region_name=region)
    s3.head_bucket(Bucket=bucket)
    n = s3.list_objects_v2(Bucket=bucket, MaxKeys=50).get("KeyCount", 0)
    # A canary write proves the key can actually upload — head/list alone cannot, and a run that
    # discovers a write failure after creating certificates would strand them.
    canary = "_preflight/canary"
    s3.put_object(Bucket=bucket, Key=canary, Body=b"canary")
    s3.delete_object(Bucket=bucket, Key=canary)
    print(f"  bucket {bucket}: reachable, write + delete OK (canary), {n} object(s)")
    print("preflight", "OK" if ok else "FAILED")
    sys.exit(0 if ok else 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true",
                    help="preflight only: verify auth, bundle id, existing certificates and bucket access; create nothing")
    ap.add_argument("--reuse-certs", metavar="DIR", help="decrypted certs/ dir from a previous run; make profiles only")
    ap.add_argument("--force-new-certs", action="store_true", help="create certificates even if the account already has that type")
    ap.add_argument("--profile", action="append", metavar="KIND:BUNDLE_ID:NAME",
                    help="a profile to make (repeatable; KIND iOS|macOS|tvOS); default: all three for APP_BUNDLE_ID")
    a = ap.parse_args()

    asc = ASC(env("ASC_KEY_ID"), env("ASC_ISSUER_ID"), env("ASC_KEY_P8"))
    app_name, slug, bundle_id = env("APP_NAME"), env("APP_SLUG"), env("APP_BUNDLE_ID")
    endpoint, bucket = env("B2_SIGNING_ENDPOINT"), env("B2_SIGNING_BUCKET")
    env("SIGNING_PASSPHRASE"); env("AWS_ACCESS_KEY_ID"); env("AWS_SECRET_ACCESS_KEY")
    specs = profile_specs(a, app_name, bundle_id)
    if a.check:
        preflight(asc, specs, endpoint, bucket)

    # Every refusal happens here, before the first write to Apple.
    print("checks:")
    specs = [(kind, resolve_bundle_id(asc, bundle_id), name) for kind, bundle_id, name in specs]
    if a.reuse_certs:
        cert_id = dist_cert_id(asc, a.reuse_certs)
        print(f"  reusing certificates from {a.reuse_certs} (distribution cert {cert_id})")
    else:
        refuse_if_certs_exist(asc, a.force_new_certs)

    work = tempfile.mkdtemp(prefix="signing-bootstrap-")
    try:
        if not a.reuse_certs:
            # From the first POST until the upload lands, the keys live only in this run: any failure in
            # between rescues what exists (encrypted) instead of shredding it.
            print("certificates:")
            try:
                _, made = create_certs(asc, work)
            except Exception as e:  # noqa: BLE001
                rescue_certs(work, f"certificate creation failed part-way: {e}")
            cert_id = made["distribution"]["id"]
            print(f"  NOTE installer certificate subject: {made['installer']['subject']}\n"
                  f"       (fastlane reads the installer identity back from the keychain, so no setting "
                  f"depends on this name.)")
            print("bundle (certificates):")
            try:
                upload(encrypt_dir(work, "certs"), "team/certs.tar.enc", endpoint, bucket)
            except Exception as e:  # noqa: BLE001
                rescue_certs(work, f"upload of team/certs.tar.enc failed: {e}")

        print("profiles:")
        create_profiles(asc, work, specs, cert_id)
        print("bundle (profiles):")
        enc = encrypt_dir(work, "profiles")
        try:
            upload(enc, f"{slug}/profiles.tar.enc", endpoint, bucket)
        except Exception as e:  # noqa: BLE001
            sys.exit(f"upload of {slug}/profiles.tar.enc failed: {e}\n"
                     f"The certificates are already safe in the bucket; re-run with --reuse-certs DIR "
                     f"(decrypt team/certs.tar.enc into DIR first) to remake only the profiles.")
    finally:
        shred(work)
        print("work dir overwritten and removed; nothing in plaintext was written anywhere else")


if __name__ == "__main__":
    main()
