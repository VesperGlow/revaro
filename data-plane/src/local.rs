use crate::error::ApiError;
use std::{
    env,
    fs::File,
    path::{Component, Path, PathBuf},
};

#[derive(Clone)]
pub struct LocalState {
    root: PathBuf,
}
impl LocalState {
    pub fn from_env() -> Result<Self, std::io::Error> {
        let root = PathBuf::from(env::var("APP_DATA_DIR").unwrap_or_else(|_| "/data".into()))
            .join("objects");
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root: root.canonicalize()?,
        })
    }
    pub fn open(&self, key: &str) -> Result<File, ApiError> {
        if key.is_empty()
            || key.contains('\\')
            || Path::new(key)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(ApiError::bad_request("invalid object key"));
        }
        let path = self
            .root
            .join(key)
            .canonicalize()
            .map_err(ApiError::internal)?;
        if !path.starts_with(&self.root) {
            return Err(ApiError::bad_request("object escapes local storage"));
        }
        File::open(path).map_err(ApiError::internal)
    }
}
