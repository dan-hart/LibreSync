use std::path::PathBuf;

#[derive(Debug)]
pub struct LaunchOptions {
    pub development_root: Option<PathBuf>,
    pub file_keys: bool,
    pub device_name: Option<String>,
}
impl LaunchOptions {
    pub fn parse(
        args: &[String],
        environment: &[(String, String)],
        debug: bool,
    ) -> Result<Self, String> {
        let env = |name: &str| {
            environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        let cli_root = args.iter().any(|a| a == "--development-root");
        let cli_file = args.iter().any(|a| a == "--development-file-keys");
        let env_root = env("LIBRESYNC_DEVELOPMENT_ROOT");
        let env_file = env("LIBRESYNC_DEVELOPMENT_FILE_KEYS");
        let env_name = env("LIBRESYNC_DEVELOPMENT_DEVICE_NAME");
        if !debug
            && (cli_root
                || cli_file
                || env_root.is_some()
                || env_file.is_some()
                || env_name.is_some())
        {
            return Err("Development storage options are unavailable in production builds".into());
        }
        let option = |name: &str| -> Result<Option<String>, String> {
            match args.iter().position(|a| a == name) {
                Some(index) => args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .cloned()
                    .map(Some)
                    .ok_or_else(|| format!("{name} requires a value")),
                None => Ok(None),
            }
        };
        let root = option("--development-root")?.or(env_root);
        let development_root = root.map(PathBuf::from);
        if development_root
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err("Development root must be an explicit absolute isolated directory".into());
        }
        let file_keys = cli_file
            || match env_file.as_deref() {
                None | Some("0") => false,
                Some("1") => true,
                Some(_) => return Err("LIBRESYNC_DEVELOPMENT_FILE_KEYS must be 0 or 1".into()),
            };
        if file_keys && development_root.is_none() {
            return Err(
                "Explicit development file keys require an isolated development root".into(),
            );
        }
        let device_name = option("--device-name")?.or(env_name);
        if device_name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
        {
            return Err("Device name cannot be empty".into());
        }
        Ok(Self {
            development_root,
            file_keys,
            device_name,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn environment() -> Vec<(String, String)> {
        [
            ("LIBRESYNC_DEVELOPMENT_ROOT", "/private/tmp/isolated-smoke"),
            ("LIBRESYNC_DEVELOPMENT_FILE_KEYS", "1"),
            ("LIBRESYNC_DEVELOPMENT_DEVICE_NAME", "UI smoke"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    #[test]
    fn explicit_debug_environment_uses_isolated_storage() {
        let options = LaunchOptions::parse(&[], &environment(), true).unwrap();
        assert_eq!(
            options.development_root,
            Some(PathBuf::from("/private/tmp/isolated-smoke"))
        );
        assert!(options.file_keys);
        assert_eq!(options.device_name.as_deref(), Some("UI smoke"));
    }
    #[test]
    fn production_rejects_every_development_environment_knob() {
        for entry in environment() {
            assert!(LaunchOptions::parse(&[], &[entry], false).is_err());
        }
        assert!(LaunchOptions::parse(
            &["--development-root".into(), "/tmp/test".into()],
            &[],
            false
        )
        .is_err());
    }
    #[test]
    fn file_keys_require_explicit_root_and_valid_flag() {
        assert!(LaunchOptions::parse(
            &[],
            &[("LIBRESYNC_DEVELOPMENT_FILE_KEYS".into(), "1".into())],
            true
        )
        .is_err());
        let mut env = environment();
        env[1].1 = "yes".into();
        assert!(LaunchOptions::parse(&[], &env, true).is_err());
        let ordinary = LaunchOptions::parse(&[], &[], true).unwrap();
        assert!(!ordinary.file_keys);
        assert!(ordinary.development_root.is_none());
    }
}
