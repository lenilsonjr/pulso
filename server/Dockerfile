FROM rust:alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
# The build context is server/, so the workspace's Cargo.lock and release
# profile are not here: the profile is set below and dependencies are resolved
# at build time.
COPY Cargo.toml ./
COPY src ./src
ENV CARGO_PROFILE_RELEASE_LTO=true \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_PROFILE_RELEASE_PANIC=abort \
    CARGO_PROFILE_RELEASE_STRIP=true
RUN cargo build --release

FROM scratch
COPY --from=build /src/target/release/pulso-server /pulso-server
ENV PULSO_DATA=/data PULSO_PORT=8787 PULSO_BIND=0.0.0.0
VOLUME /data
EXPOSE 8787
ENTRYPOINT ["/pulso-server"]
