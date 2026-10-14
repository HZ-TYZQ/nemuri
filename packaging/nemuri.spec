Name:           nemuri
Version:        2.0.0
Release:        1%{?dist}
Summary:        Terminal player for ASMR and music

License:        MIT AND Apache-2.0 AND MPL-2.0 AND BSD-3-Clause
URL:            https://github.com/HZ-TYZQ/nemuri
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  desktop-file-utils
BuildRequires:  pkgconfig(alsa)
BuildRequires:  pkgconfig(sqlite3)

ExclusiveArch:  x86_64

# 1.4.0 及以前叫 music-player；版本高于它时直接替换旧包。
Provides:       music-player = %{version}-%{release}
Obsoletes:      music-player < %{version}-%{release}

%description
Nemuri is a terminal player for long ASMR recordings and ordinary music
from a local library. It uses Rodio for playback, Lofty for metadata,
SQLite for an incremental index that can be rebuilt, and XDG directories
for settings and playlists. On Linux it exposes MPRIS 2 for desktop media
controls.

%prep
%autosetup -n %{name}-%{version}
tar -xzf %{SOURCE1}

%build
export CARGO_HOME="$PWD/.cargo-home"
export CARGO_TARGET_DIR="$PWD/.cargo-target"
mkdir -p "$CARGO_HOME"
cp vendor-config.toml "$CARGO_HOME/config.toml"
cargo build --release --locked --offline

%check
export CARGO_HOME="$PWD/.cargo-home"
export CARGO_TARGET_DIR="$PWD/.cargo-target"
cargo test --release --locked --offline
desktop-file-validate packaging/%{name}.desktop

%install
install -Dpm 0755 .cargo-target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dpm 0644 packaging/%{name}.1 %{buildroot}%{_mandir}/man1/%{name}.1
install -Dpm 0644 packaging/%{name}.desktop %{buildroot}%{_datadir}/applications/%{name}.desktop
install -Dpm 0644 assets/icons/%{name}.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg
install -Dpm 0644 assets/icons/%{name}-48.png %{buildroot}%{_datadir}/icons/hicolor/48x48/apps/%{name}.png

%files
%license LICENSE packaging/licenses/Apache-2.0.txt packaging/licenses/MPL-2.0.txt
%doc README.md changelog.md packaging/licenses/THIRD-PARTY-NOTICES.txt
%{_bindir}/%{name}
%{_mandir}/man1/%{name}.1*
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg
%{_datadir}/icons/hicolor/48x48/apps/%{name}.png

%changelog
* Wed Oct 14 2026 HZ-TYZQ - 2.0.0-1
- Rename Music Player to Nemuri and migrate settings and user data
- Replace the old music-player RPM on upgrade
- Display file names and filter the library and playback by folder
- Add sleep timers with fade-out and resume progress for long recordings
- Add stop-after-track mode, media-control privacy, and channel controls
- Fix queue reordering, playback position, and startup warning display

* Tue Oct 06 2026 HZ-TYZQ - 1.4.0-1
- Move settings to a hand-editable config.kdl; on exit only the settings
  changed while running are written back, keeping comments and layout
- Carry over config.toml on first start and report config mistakes by line
- Add default, light, and terminal theme presets with per-color overrides
- Abstract the playback backend and route input through actions

* Tue Oct 06 2026 HZ-TYZQ - 1.3.4-1
- Show plain lyrics without timestamps, scrolled with playback progress
- Fall back to embedded lyrics when the sidecar LRC file is unusable

* Tue Oct 06 2026 HZ-TYZQ - 1.3.3-1
- Keep the lyrics pane beside the library even for tracks without lyrics
- Show library tracks on two rows: title and duration, then artist,
  album and format

* Mon Sep 21 2026 HZ-TYZQ - 1.3.2-1
- Add synchronized LRC lyrics from sidecar files or embedded tags,
  decoding UTF-8, UTF-16 and GBK
- Resume the last track paused at its saved position on startup
- Add 60-second seek, percentage jump and 1% volume keys

* Fri Sep 18 2026 HZ-TYZQ - 1.3.1-1
- Preserve playback on audio output failure and allow manual recovery
- Refresh playback action symbols

* Sun Sep 07 2026 HZ-TYZQ - 1.3.0-1
- Add a play queue panel with jump-to, remove, reorder and clear
- Sort the library by path, title, artist, album or duration in either direction
- Read track and disc numbers so album order follows the album itself
- Add mouse support for lists, the wheel and the progress bar
- Keep the reason a track was skipped instead of clearing it on the next track

* Mon Aug 31 2026 HZ-TYZQ - 1.2.2-1
- Redesign the application icon as a restrained monochrome spectrum
- Add a dedicated small-size icon and regenerate PNG and Windows ICO assets
- Avoid Windows media-session startup and shutdown deadlocks

* Thu Aug 20 2026 HZ-TYZQ - 1.2.1-1
- Skip broken tracks correctly and keep playback history clean
- Encode MPRIS file URLs and clear stale Windows SMTC metadata
- Split application and UI code into focused modules

* Tue Aug 18 2026 HZ-TYZQ - 1.2.0-1
- Add Linux MPRIS and Windows SMTC media session integration
- Split playback mode into repeat and shuffle; reshuffle each random round
- Keep pause position; keep paused state when changing tracks

* Mon Aug 17 2026 HZ-TYZQ - 1.1.0-1
- Replace GStreamer playback and Discoverer with Rodio and Lofty
- Rebuild the spectrum from a PCM tap and rustfft
- Build against ALSA; let rpmbuild generate runtime ELF dependencies
- Ship Apache-2.0, MPL-2.0, and third-party notices

* Sat Aug 15 2026 HZ-TYZQ - 1.0.2-1
- Add Windows 11 x86_64 MSVC builds, tests, and release packaging
- Bundle a private GStreamer runtime and SQLite in Windows packages
- Keep Fedora linked to its system GStreamer and SQLite libraries

* Sat Aug 15 2026 HZ-TYZQ - 1.0.1-1
- Add desktop integration and a restrained monochrome terminal theme
- Preserve the selected track after background rescans with active fuzzy search
- Try the next track after asynchronous playback errors

* Fri Aug 14 2026 HZ-TYZQ - 1.0.0-1
- Publish the first stable release with playback, library management, playlists, search, and audio visualization
- Keep asynchronous Nucleo search results synchronized with matcher snapshots

* Fri Aug 14 2026 HZ-TYZQ - 0.3.0-3
- Reduce low-frequency visualizer saturation with interpolated sampling and display headroom

* Fri Aug 14 2026 HZ-TYZQ - 0.3.0-2
- Refine the spectrum to 50-8000 Hz logarithmic bars and action-oriented playback icons

* Fri Aug 14 2026 HZ-TYZQ - 0.3.0-1
- Add an integrated GStreamer spectrum visualizer with a persistent toggle

* Fri Aug 14 2026 HZ-TYZQ - 0.2.0-1
- Add GStreamer playback, background library index, search, queue, and playlists

* Fri Aug 14 2026 HZ-TYZQ - 0.1.0-1
- Initial Fedora 44 RPM package
