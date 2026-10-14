//! 改名为 Nemuri 之前，设置和用户数据放在 `tui-music-player` 目录里。
//!
//! 新目录还不存在时，第一次启动把旧目录整个复制过来，旧目录原样保留：
//! 退回旧版本时一切照旧。缓存随时可以重建，不复制。

use std::fs;
use std::io;
use std::path::Path;

use super::AppPaths;

/// 改名之前的目录名。
pub(super) const PREVIOUS_APP_DIR: &str = "tui-music-player";

/// 新目录还不存在、旧目录存在时复制过来，返回要在底栏显示的说明；没有可复制的就返回 None。
///
/// 复制失败时返回错误，程序不应继续启动：否则会在新目录里从头开始，
/// 之后再也不会复制，设置和播放记录就悄悄分成了两份。
pub fn adopt_previous_dirs(paths: &AppPaths) -> Result<Option<String>, String> {
    let mut copied = false;
    for (old, new) in &paths.renamed_dirs {
        if new.exists() || !old.is_dir() {
            continue;
        }
        copy_into_place(old, new).map_err(|error| {
            format!(
                "无法把旧目录 {} 复制到 {}: {error}。旧目录没有改动，处理好后再启动",
                old.display(),
                new.display()
            )
        })?;
        copied = true;
    }
    Ok(copied.then(|| {
        format!(
            "已把 {PREVIOUS_APP_DIR} 目录里的设置和播放记录复制到新目录；旧目录原样保留，确认无误后可以删除"
        )
    }))
}

/// 先复制到旁边的临时目录，完整复制后再改名成新目录：中途退出时新目录不存在，
/// 下次启动会重新复制，上次留下的临时目录先删掉。
fn copy_into_place(old: &Path, new: &Path) -> io::Result<()> {
    let mut staging = new.as_os_str().to_owned();
    staging.push(".migrating");
    let staging = Path::new(&staging);
    if fs::symlink_metadata(staging).is_ok() {
        remove_staging(staging)?;
    }
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent)?;
    }
    let result = copy_tree(old, staging).and_then(|()| fs::rename(staging, new));
    if result.is_err() {
        // 删不掉也不要紧：下次启动会再删一次，而复制失败的原因更值得报告。
        let _ = remove_staging(staging);
    }
    result
}

/// 删掉复制到一半的临时目录。它照旧目录复制了权限，可能有只读的目录；在 Unix 上
/// 直接递归删除会因为没有写权限而失败。这棵树是迁移自己建的，先让各级目录可写再删。
/// Windows 上标准库删除时不理会只读属性，不用先处理。
fn remove_staging(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    make_dirs_writable(path)?;
    fs::remove_dir_all(path)
}

#[cfg(unix)]
fn make_dirs_writable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // 文件能不能删只看所在目录；符号链接不跟进去。
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(path, permissions)?;
    for entry in fs::read_dir(path)? {
        make_dirs_writable(&entry?.path())?;
    }
    Ok(())
}

/// 递归复制目录。符号链接按它指向的内容复制，程序自己从不在这里创建链接。
///
/// 各级目录保持源目录的权限：先建成只有自己能访问的目录，内容复制完再改成源目录的权限，
/// 复制途中和复制之后都不会比原来更开放。文件的权限由 `fs::copy` 一并复制。
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    create_private_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if fs::metadata(&source)?.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target)?;
        }
    }
    fs::set_permissions(to, fs::metadata(from)?.permissions())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::AppConfig;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    /// Linux 的布局：配置、数据、缓存各在一处。
    fn linux_paths(root: &Path) -> AppPaths {
        AppPaths::from_roots(
            root.join("config"),
            root.join("data"),
            root.join("cache"),
            None,
        )
    }

    /// 旧版本留下的配置、播放列表、会话、进度和缓存。
    fn seed_previous(root: &Path) {
        write(
            &root.join("config/tui-music-player/config.kdl"),
            "playback {\n    volume 35\n}\n",
        );
        write(
            &root.join("data/tui-music-player/playlists/夜里.json"),
            "{\"tracks\":[]}",
        );
        write(
            &root.join("data/tui-music-player/session.toml"),
            "version = 1\n",
        );
        write(&root.join("data/tui-music-player/progress.json"), "{}");
        write(
            &root.join("cache/tui-music-player/library.sqlite3"),
            "cache",
        );
    }

    #[test]
    fn the_first_start_copies_settings_and_data_and_keeps_the_old_folders() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        seed_previous(root);
        let paths = linux_paths(root);

        let message = adopt_previous_dirs(&paths).unwrap().unwrap();
        assert!(message.contains("复制到新目录"), "{message}");
        assert_eq!(read(&paths.config_file), "playback {\n    volume 35\n}\n");
        assert_eq!(
            read(&paths.playlists_dir.join("夜里.json")),
            "{\"tracks\":[]}"
        );
        assert_eq!(read(&paths.session_file), "version = 1\n");
        assert_eq!(read(&paths.progress_file), "{}");
        // 缓存可以重建，不复制；旧目录一个文件也不少。
        assert!(!root.join("cache/nemuri").exists());
        assert_eq!(
            read(&root.join("config/tui-music-player/config.kdl")),
            "playback {\n    volume 35\n}\n"
        );
        assert!(root.join("data/tui-music-player/progress.json").exists());
        assert!(!root.join("config/nemuri.migrating").exists());

        // 复制过来的配置照常读取。
        assert_eq!(AppConfig::load(&paths).unwrap().config.volume, 35);
    }

    #[test]
    fn nothing_is_copied_once_the_new_folders_exist() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        seed_previous(root);
        let paths = linux_paths(root);
        adopt_previous_dirs(&paths).unwrap();
        write(&paths.config_file, "playback {\n    volume 80\n}\n");

        assert_eq!(adopt_previous_dirs(&paths).unwrap(), None);
        assert_eq!(read(&paths.config_file), "playback {\n    volume 80\n}\n");
    }

    #[test]
    fn a_fresh_install_has_nothing_to_copy() {
        let temp = tempfile::tempdir().unwrap();
        let paths = linux_paths(temp.path());
        assert_eq!(adopt_previous_dirs(&paths).unwrap(), None);
        assert!(!temp.path().join("config").exists());
    }

    #[test]
    fn a_shared_config_and_data_root_is_copied_once() {
        // Windows 上配置和数据都在 %APPDATA%。
        let temp = tempfile::tempdir().unwrap();
        let roaming = temp.path().join("Roaming");
        write(
            &roaming.join("tui-music-player/config.kdl"),
            "sort \"title\"\n",
        );
        write(&roaming.join("tui-music-player/progress.json"), "{}");
        let paths = AppPaths::from_roots(
            roaming.clone(),
            roaming.clone(),
            temp.path().join("Local"),
            None,
        );
        assert_eq!(paths.renamed_dirs.len(), 1);

        assert!(adopt_previous_dirs(&paths).unwrap().is_some());
        assert_eq!(read(&paths.config_file), "sort \"title\"\n");
        assert_eq!(read(&paths.progress_file), "{}");
    }

    #[test]
    fn a_copy_cut_short_last_time_starts_over() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        seed_previous(root);
        write(&root.join("config/nemuri.migrating/half-copied"), "x");
        let paths = linux_paths(root);

        assert!(adopt_previous_dirs(&paths).unwrap().is_some());
        assert!(!root.join("config/nemuri/half-copied").exists());
        assert!(!root.join("config/nemuri.migrating").exists());
        assert!(paths.config_file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_copy_stops_the_start_and_leaves_no_new_folder() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        seed_previous(root);
        let locked = root.join("data/tui-music-player/progress.json");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            // 以 root 运行（例如 RPM 的 %check）时权限挡不住读取，这个场景无从模拟。
            return;
        }
        let paths = linux_paths(root);

        let error = adopt_previous_dirs(&paths).unwrap_err();
        assert!(error.contains("旧目录没有改动"), "{error}");
        assert!(!root.join("data/nemuri").exists());
        assert!(!root.join("data/nemuri.migrating").exists());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copied_folders_keep_their_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        seed_previous(root);
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let set_mode = |path: &Path, mode: u32| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        };
        // 私有的数据目录和子目录，以及一个普通的配置目录和一个私有文件。
        let old_data = root.join("data/tui-music-player");
        set_mode(&old_data, 0o700);
        set_mode(&old_data.join("playlists"), 0o700);
        set_mode(&old_data.join("session.toml"), 0o600);
        set_mode(&root.join("config/tui-music-player"), 0o755);
        let paths = linux_paths(root);

        adopt_previous_dirs(&paths).unwrap();
        assert_eq!(mode(&root.join("data/nemuri")), 0o700, "data/nemuri");
        assert_eq!(mode(&paths.playlists_dir), 0o700, "playlists");
        assert_eq!(mode(&paths.session_file), 0o600, "session.toml");
        assert_eq!(mode(&root.join("config/nemuri")), 0o755, "config/nemuri");
    }

    #[cfg(unix)]
    #[test]
    fn folders_are_private_while_they_are_being_copied() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("staging");
        create_private_dir(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_interrupted_copy_of_a_read_only_folder_starts_over() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        seed_previous(root);
        let old_config = root.join("config/tui-music-player");
        let set_mode = |path: &Path, mode: u32| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        };
        set_mode(&old_config, 0o500);
        // 上次复制完了、还没改名就退出了：临时目录照旧目录的权限变成 0500。
        copy_tree(&old_config, &root.join("config/nemuri.migrating")).unwrap();
        let paths = linux_paths(root);

        let result = adopt_previous_dirs(&paths);
        set_mode(&old_config, 0o755);
        assert!(result.unwrap().is_some());
        assert!(paths.config_file.exists());
        assert!(!root.join("config/nemuri.migrating").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_staging_tree_with_read_only_folders_is_removed() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join("nemuri.migrating");
        write(&staging.join("playlists/夜里.json"), "{}");
        write(&staging.join("config.kdl"), "");
        for (path, mode) in [
            (staging.join("playlists/夜里.json"), 0o400),
            (staging.join("playlists"), 0o500),
            (staging.clone(), 0o500),
        ] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }

        remove_staging(&staging).unwrap();
        assert!(!staging.exists());
    }

    #[test]
    fn renamed_dirs_pair_each_old_folder_with_its_new_one() {
        let paths = linux_paths(Path::new("/root"));
        assert_eq!(
            paths.renamed_dirs,
            [
                (
                    PathBuf::from("/root/config/tui-music-player"),
                    PathBuf::from("/root/config/nemuri")
                ),
                (
                    PathBuf::from("/root/data/tui-music-player"),
                    PathBuf::from("/root/data/nemuri")
                ),
            ]
        );
    }
}
