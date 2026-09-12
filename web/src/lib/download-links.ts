export const SOURCE_URL = "https://github.com/Dave136/nox";
export const RELEASE_URL = "https://github.com/Dave136/nox/releases/latest";

export const DOWNLOADS = {
  macos: {
    platform: "macOS",
    label: "Download for macOS",
    shortLabel: "macOS",
    fileName: "nox-macos-aarch64.zip",
    href: "https://github.com/Dave136/nox/releases/latest/download/nox-macos-aarch64.zip",
    arch: "Apple Silicon",
    format: "ZIP",
    note: "For M1, M2, M3, M4, and newer Macs.",
  },
  linux: {
    platform: "Linux",
    label: "Download for Linux",
    shortLabel: "Linux",
    fileName: "nox-linux-x86_64.tar.gz",
    href: "https://github.com/Dave136/nox/releases/latest/download/nox-linux-x86_64.tar.gz",
    arch: "x86_64",
    format: "tar.gz",
    note: "For Linux desktops and workstations on x86_64.",
  },
} as const;

export const primaryDownload = DOWNLOADS.macos;
export const downloadOptionsPath = "/download";
