use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

const SOURCE: &str = include_str!("../../../clients/python/bevy_feedback.py");

/// Bundled Python client materialized on disk.
pub(crate) struct BundledPythonClient {
    dir: PathBuf,
}

impl BundledPythonClient {
    pub(crate) fn materialize(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        fs::write(dir.join("bevy_feedback.py"), SOURCE)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// `PYTHONPATH` with the bundled client first; any user value follows.
    pub(crate) fn python_path(&self) -> OsString {
        let mut path = self.dir.as_os_str().to_os_string();
        if let Some(existing) = std::env::var_os("PYTHONPATH").filter(|value| !value.is_empty()) {
            path.push(if cfg!(windows) { ";" } else { ":" });
            path.push(existing);
        }
        path
    }

    pub(crate) fn remove(self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
