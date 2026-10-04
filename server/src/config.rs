use std::path::PathBuf;

const DEFAULT_BIND: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8787;
const DEFAULT_DATA: &str = "./data";

pub const NO_TOKEN: &str = "PULSO_TOKEN is empty or unset. Set it to the bearer token the app sends; \
the server does not run without authentication.";

/// `var` returns an environment variable, or `None` when it is unset or empty.
pub type Var<'a> = &'a dyn Fn(&str) -> Option<String>;

pub struct Dirs {
    pub data: PathBuf,
    pub index: PathBuf,
}

pub struct Listen {
    pub bind: String,
    pub port: u16,
}

pub fn dirs(var: Var) -> Dirs {
    let data = PathBuf::from(var("PULSO_DATA").unwrap_or_else(|| DEFAULT_DATA.to_owned()));
    let index = var("PULSO_INDEX").map_or_else(|| data.join(".index"), PathBuf::from);
    Dirs { data, index }
}

pub fn listen(var: Var) -> Result<Listen, String> {
    let bind = var("PULSO_BIND").unwrap_or_else(|| DEFAULT_BIND.to_owned());
    let port = match var("PULSO_PORT") {
        None => DEFAULT_PORT,
        Some(text) => text.trim().parse().map_err(|_| {
            format!("PULSO_PORT must be a port number from 0 to 65535, got {text:?}")
        })?,
    };
    Ok(Listen { bind, port })
}

pub fn token(var: Var) -> Result<String, String> {
    var("PULSO_TOKEN")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| NO_TOKEN.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |name| {
            map.get(name)
                .map(|v| (*v).to_owned())
                .filter(|v| !v.is_empty())
        }
    }

    #[test]
    fn defaults_bind_to_loopback_on_8787() {
        let listen = listen(&env(&[])).unwrap();
        assert_eq!((listen.bind.as_str(), listen.port), ("127.0.0.1", 8787));
    }

    #[test]
    fn bind_and_port_come_from_the_environment() {
        let listen = listen(&env(&[("PULSO_BIND", "0.0.0.0"), ("PULSO_PORT", "9001")])).unwrap();
        assert_eq!((listen.bind.as_str(), listen.port), ("0.0.0.0", 9001));
    }

    #[test]
    fn an_empty_variable_counts_as_unset() {
        let listen = listen(&env(&[("PULSO_BIND", ""), ("PULSO_PORT", "")])).unwrap();
        assert_eq!((listen.bind.as_str(), listen.port), ("127.0.0.1", 8787));
    }

    #[test]
    fn a_bad_port_is_reported() {
        for bad in ["http", "70000", "-1", "80.5"] {
            let err = listen(&env(&[("PULSO_PORT", bad)])).err().unwrap();
            assert!(err.contains("PULSO_PORT"), "{err}");
        }
    }

    #[test]
    fn the_data_directory_is_configurable_and_the_index_follows_it() {
        let dirs = dirs(&env(&[("PULSO_DATA", "/srv/pulso")]));
        assert_eq!(dirs.data, PathBuf::from("/srv/pulso"));
        assert_eq!(dirs.index, PathBuf::from("/srv/pulso/.index"));
        assert_eq!(super::dirs(&env(&[])).data, PathBuf::from("./data"));
    }

    #[test]
    fn the_index_directory_can_be_moved_out_of_the_data_directory() {
        let dirs = dirs(&env(&[
            ("PULSO_DATA", "/srv/pulso"),
            ("PULSO_INDEX", "/var/cache/pulso"),
        ]));
        assert_eq!(dirs.index, PathBuf::from("/var/cache/pulso"));
    }

    #[test]
    fn an_unset_empty_or_blank_token_is_refused() {
        for value in [None, Some(""), Some("   "), Some("\t\n")] {
            let pairs: Vec<(&str, &str)> = value.map(|v| ("PULSO_TOKEN", v)).into_iter().collect();
            let err = token(&env(&pairs)).err().unwrap();
            assert!(err.contains("PULSO_TOKEN"), "{err}");
        }
    }

    #[test]
    fn a_token_is_kept_without_surrounding_whitespace() {
        assert_eq!(
            token(&env(&[("PULSO_TOKEN", " secret\n")])).unwrap(),
            "secret"
        );
    }
}
