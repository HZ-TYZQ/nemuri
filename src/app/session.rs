//! 退出时记住当前曲目、进度和文件夹筛选，下次启动时回到原来的文件夹，
//! 暂停在原处，按空格续播。

use std::path::Path;
use std::time::Duration;

use crate::player::PlayState;
use crate::progress::NEAR_END;
use crate::session::Session;

use super::App;

impl App {
    /// 在第一次扫描完成后调用：曲库就绪才能按路径找到曲目和文件夹。
    /// 文件夹已经不在了就显示全部；用户已经开始播放时不打断；
    /// 找不到曲目或打不开都安静放弃。
    pub(super) fn restore_session(&mut self) {
        let Some(session) = self.pending_session.take() else {
            return;
        };
        if let Some(folder) = session.folder
            && self
                .catalog
                .folders()
                .iter()
                .any(|(known, _)| *known == folder)
        {
            self.set_folder(Some(folder));
        }
        let Some(track) = session.track else {
            return;
        };
        if self.playback.player.state() != PlayState::Stopped
            || self.playback.playing_index.is_some()
        {
            return;
        }
        let Some(index) = self.index_for_path(&track) else {
            return;
        };
        let mut position = session.position;
        if let Some(duration) = self.catalog.tracks()[index].duration
            && position + NEAR_END >= duration
        {
            position = Duration::ZERO;
        }
        // 位置恢复失败（例如文件被改短）时退回曲首，仍然选中这首歌。
        let opened = self
            .playback
            .player
            .open_at(&track, position)
            .map(|()| position)
            .or_else(|_| self.playback.player.open(&track).map(|()| Duration::ZERO));
        let Ok(position) = opened else {
            return;
        };

        self.playback.spectrum.on_track_change();
        self.playback.playing_index = Some(index);
        self.view.pending_selected_path = Some(track);
        self.restore_pending_selection();
        if self.config.shuffle {
            self.reanchor_shuffle_bag(index);
        }
        self.view.message = Some(format!(
            "已恢复上次播放：{} {}，按空格继续",
            self.catalog.tracks()[index].display_title(),
            crate::track::fmt_duration(position)
        ));
    }

    /// 退出时记录当前曲目、进度和文件夹筛选；两者都没有时清掉旧会话。
    /// 还没来得及恢复就退出时保留原文件，不能把上次的会话抹掉。
    pub fn save_session(&self) -> Result<(), String> {
        if self.pending_session.is_some() {
            return Ok(());
        }
        let path = &self.paths.session_file;
        let track = (self.playback.player.state() != PlayState::Stopped)
            .then(|| self.playback.player.current_path())
            .flatten();
        let folder = self.catalog.folder().cloned();
        let result = if track.is_none() && folder.is_none() {
            Session::clear(path)
        } else {
            Session {
                library_dir: self.library_dir.clone(),
                track: track.map(Path::to_path_buf),
                position: track.map_or(Duration::ZERO, |_| self.playback.player.position()),
                folder,
            }
            .save(path)
        };
        result.map_err(|error| format!("无法保存播放会话: {error}"))
    }
}
