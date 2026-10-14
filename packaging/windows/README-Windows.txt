Nemuri for Windows 11 x86_64
============================

启动
----

安装版可以从开始菜单启动。Portable ZIP 解压后运行 nemuri.exe
或 nemuri.cmd。在 Windows Terminal 中也可以执行：

    .\nemuri.exe

使用 --set-library 可以永久设置音乐库：

    .\nemuri.exe --set-library "D:\Music"

用户数据
--------

配置和播放列表保存在 %APPDATA%\nemuri，缓存保存在
%LOCALAPPDATA%\nemuri。Portable ZIP 也使用这些标准目录。
卸载应用不会删除这些用户数据。

1.4.0 及以前叫 Music Player，数据在 %APPDATA%\tui-music-player。
第一次启动 Nemuri 时会把它复制到 %APPDATA%\nemuri，旧目录原样保留，
确认无误后可以删除；缓存会重建。安装程序会原地升级已安装的
Music Player，沿用原来的安装目录和 PATH 设置。

运行依赖
--------

播放使用 Rodio / WASAPI，媒体信息使用 Lofty，SQLite 已编入程序。
用户不需要另外安装 GStreamer 或 SQLite。

许可
----

第三方 Rust crate 的许可证说明见 THIRD-PARTY-NOTICES.txt。
Apache-2.0 全文见 third-party-licenses\Apache-2.0.txt。
MPL-2.0 全文见 third-party-licenses\MPL-2.0.txt。

安全提示
--------

第一版 Windows 发行包尚未使用 Authenticode 签名。Windows Defender
SmartScreen 可能显示“Windows 已保护你的电脑”。请只从项目的 GitHub Release
下载，并用同一 Release 中的 SHA256SUMS.txt 核对文件。

项目主页：https://github.com/HZ-TYZQ/nemuri
版权属名：HZ-TYZQ
