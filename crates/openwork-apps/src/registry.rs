use crate::{AppId, AvailableApp, CapabilityPack};
use openwork_core::{ErrorCode, OpenWorkError};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct AppRegistry {
    apps: BTreeMap<AppId, AvailableApp>,
}

impl AppRegistry {
    /// Loads deterministic YAML manifests from one administrator-controlled directory.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable directories, invalid manifests, duplicates, or excess entries.
    pub fn load(directory: &Path) -> Result<Self, OpenWorkError> {
        let mut paths = fs::read_dir(directory)
            .map_err(|_| registry_error("app registry directory is unavailable"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "yaml")
            })
            .collect::<Vec<_>>();
        paths.sort();
        if paths.len() > 128 {
            return Err(registry_error("app registry contains too many manifests"));
        }
        let mut apps = BTreeMap::new();
        for path in paths {
            let input = fs::read_to_string(path)
                .map_err(|_| registry_error("app registry manifest is unreadable"))?;
            let app = CapabilityPack::from_yaml(&input)?.into_available();
            if apps.insert(app.metadata.id.clone(), app).is_some() {
                return Err(registry_error(
                    "app registry contains duplicate app identifiers",
                ));
            }
        }
        Ok(Self { apps })
    }

    #[must_use]
    pub fn list(&self) -> Vec<&AvailableApp> {
        self.apps.values().collect()
    }

    #[must_use]
    pub fn get(&self, id: &AppId) -> Option<&AvailableApp> {
        self.apps.get(id)
    }
}

fn registry_error(message: &'static str) -> OpenWorkError {
    OpenWorkError::new(ErrorCode::ConfigInvalid, message)
}
