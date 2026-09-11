#!/usr/bin/env bash
set -euo pipefail

os="${1:?usage: package-release-assets.sh <linux|macos> <target> <artifact-name> <binary-path> <version>}"
target="${2:?missing target}"
artifact_name="${3:?missing artifact name}"
binary_path="${4:?missing binary path}"
version="${5:?missing version}"

root="$(git rev-parse --show-toplevel)"
dist="${root}/dist"
work="${root}/target/package-work-${artifact_name}"
install_name="nox"
product_name="Nox"
bundle_id="dev.dave136.nox"

rm -rf "${work}"
mkdir -p "${dist}" "${work}"

copy_linux_payload() {
  local appdir="$1"
  install -Dm755 "${binary_path}" "${appdir}/usr/bin/${install_name}"
  install -Dm644 "${root}/packaging/nox.desktop" "${appdir}/usr/share/applications/nox.desktop"
  install -Dm644 "${root}/packaging/icons/hicolor/scalable/apps/nox.svg" "${appdir}/usr/share/icons/hicolor/scalable/apps/nox.svg"
  install -Dm644 "${root}/LICENSE" "${appdir}/usr/share/doc/nox/LICENSE"
}

package_linux() {
  local appdir="${work}/linux-root"
  copy_linux_payload "${appdir}"

  local debroot="${work}/deb"
  copy_linux_payload "${debroot}"
  install -d "${debroot}/DEBIAN"
  cat > "${debroot}/DEBIAN/control" <<EOF
Package: nox
Version: ${version}
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Dave136
Description: Native desktop password manager
Depends: libc6, libfontconfig1, libgcc-s1, libx11-6, libxcb1, libxcb-render0, libxcb-shape0, libxcb-xfixes0, libxkbcommon0, libxkbcommon-x11-0
EOF
  dpkg-deb --build --root-owner-group "${debroot}" "${dist}/${artifact_name}.deb"

  local rpmbuild="${work}/rpmbuild"
  mkdir -p "${rpmbuild}/BUILD" "${rpmbuild}/RPMS" "${rpmbuild}/SOURCES" "${rpmbuild}/SPECS" "${rpmbuild}/SRPMS"
  tar -czf "${rpmbuild}/SOURCES/nox-${version}.tar.gz" -C "${appdir}" usr
  cat > "${rpmbuild}/SPECS/nox.spec" <<EOF
Name: nox
Version: ${version}
Release: 1%{?dist}
Summary: Native desktop password manager
License: GPL-3.0-only
URL: https://github.com/Dave136/nox
BuildArch: x86_64
Requires: glibc, fontconfig, libX11, libxcb, libxkbcommon, libxkbcommon-x11

%description
Native desktop password manager.

%prep
%setup -q -c -T
%{__tar} -xzf %{SOURCE0}

%install
mkdir -p %{buildroot}
cp -a usr %{buildroot}/

%files
/usr/bin/nox
/usr/share/applications/nox.desktop
/usr/share/icons/hicolor/scalable/apps/nox.svg
/usr/share/doc/nox/LICENSE
EOF
  rpmbuild --define "_topdir ${rpmbuild}" -bb "${rpmbuild}/SPECS/nox.spec"
  cp "${rpmbuild}"/RPMS/x86_64/*.rpm "${dist}/${artifact_name}.rpm"

  local appimage_appdir="${work}/AppDir"
  copy_linux_payload "${appimage_appdir}"
  cp "${root}/packaging/nox.desktop" "${appimage_appdir}/nox.desktop"
  cp "${root}/packaging/icons/hicolor/scalable/apps/nox.svg" "${appimage_appdir}/nox.svg"

  local linuxdeploy="${work}/linuxdeploy-x86_64.AppImage"
  curl --fail --location --retry 3 \
    --output "${linuxdeploy}" \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"
  chmod +x "${linuxdeploy}"
  APPIMAGE_EXTRACT_AND_RUN=1 "${linuxdeploy}" \
    --appdir "${appimage_appdir}" \
    --executable "${appimage_appdir}/usr/bin/${install_name}" \
    --desktop-file "${appimage_appdir}/nox.desktop" \
    --icon-file "${appimage_appdir}/nox.svg" \
    --output appimage
  mv "${product_name}"-*.AppImage "${dist}/${artifact_name}.AppImage"
}

package_macos() {
  local app="${work}/${product_name}.app"
  local contents="${app}/Contents"
  local macos="${contents}/MacOS"
  local resources="${contents}/Resources"
  mkdir -p "${macos}" "${resources}"
  install -m 755 "${binary_path}" "${macos}/${install_name}"
  install -m 644 "${root}/LICENSE" "${resources}/LICENSE"
  cat > "${contents}/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>${install_name}</string>
  <key>CFBundleIdentifier</key>
  <string>${bundle_id}</string>
  <key>CFBundleName</key>
  <string>${product_name}</string>
  <key>CFBundleDisplayName</key>
  <string>${product_name}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleVersion</key>
  <string>${version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
EOF

  ditto -c -k --keepParent "${app}" "${dist}/${artifact_name}.zip"
  hdiutil create \
    -volname "${product_name}" \
    -srcfolder "${app}" \
    -ov \
    -format UDZO \
    "${dist}/${artifact_name}.dmg"
}

case "${os}" in
  linux)
    package_linux
    ;;
  macos)
    package_macos
    ;;
  *)
    echo "unsupported packaging os: ${os}" >&2
    exit 64
    ;;
esac
