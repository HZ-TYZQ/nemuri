//! 命令行参数定义和目录校验。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::{Parser, ValueHint};

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// 本次运行临时打开的音乐目录，不修改主音乐库配置
    #[arg(value_name = "DIRECTORY", value_hint = ValueHint::DirPath)]
    pub directory: Option<PathBuf>,

    /// 永久设置主音乐库目录并启动播放器
    #[arg(
        long,
        value_name = "PATH",
        value_hint = ValueHint::DirPath,
        conflicts_with = "directory"
    )]
    pub set_library: Option<PathBuf>,
}

pub fn validate_directory(path: &Path) -> io::Result<PathBuf> {
    let metadata = fs::metadata(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("无法访问音乐目录 {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("音乐库路径不是目录: {}", path.display()),
        ));
    }
    fs::read_dir(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("无法读取音乐目录 {}: {error}", path.display()),
        )
    })?;
    path.canonicalize().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("无法解析音乐目录 {}: {error}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_directory_and_set_library_conflict() {
        let result = Cli::try_parse_from(["nemuri", "/tmp", "--set-library", "/tmp"]);
        assert!(result.is_err());
    }

    #[test]
    fn directory_validation_rejects_files() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("not-a-directory");
        fs::write(&file, []).unwrap();
        let error = validate_directory(&file).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
