# Repackages the release tarball from build-tarball.sh; the Rust build happens there.
%global debug_package %{nil}

Name:           gantry
Version:        0.1.0
Release:        1%{?dist}
Summary:        Terminal for running and managing Claude Code sessions
License:        (MIT OR Apache-2.0) AND OFL-1.1
Source0:        gantry-%{version}-x86_64-linux.tar.gz
ExclusiveArch:  x86_64
BuildRequires:  desktop-file-utils

%description
Gantry is a GPU-rendered terminal with tabs and split panes, a tree of every
Claude Code session on the machine, status and notifications, and sessions
that survive closing the window.

%prep
%setup -q -n gantry-%{version}

%install
install -Dm0755 gantry %{buildroot}%{_bindir}/gantry
install -Dm0755 gantryd %{buildroot}%{_bindir}/gantryd
install -Dm0644 io.github.idogrf.Gantry.desktop %{buildroot}%{_datadir}/applications/io.github.idogrf.Gantry.desktop
install -Dm0644 io.github.idogrf.Gantry.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.idogrf.Gantry.svg

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.idogrf.Gantry.desktop

%files
%license LICENSE-MIT LICENSE-APACHE LICENSE-FONT-FIGTREE
%{_bindir}/gantry
%{_bindir}/gantryd
%{_datadir}/applications/io.github.idogrf.Gantry.desktop
%{_datadir}/icons/hicolor/scalable/apps/io.github.idogrf.Gantry.svg

%changelog
* Thu Oct 01 2026 Ido Greenfeld <15867904+idogrf@users.noreply.github.com> - 0.1.0-1
- First package.
