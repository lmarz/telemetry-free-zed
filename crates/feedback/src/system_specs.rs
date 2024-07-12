use gpui::{AppContext, Task};
use human_bytes::human_bytes;
use release_channel::{AppCommitSha, AppVersion, ReleaseChannel};
use serde::Serialize;
use std::{env, fmt::Display};
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

#[derive(Clone, Debug, Serialize)]
pub struct SystemSpecs {
    app_version: String,
    release_channel: &'static str,
    os_name: String,
    os_version: String,
    memory: u64,
    architecture: &'static str,
    commit_sha: Option<String>,
}

impl SystemSpecs {
    pub fn new(cx: &AppContext) -> Task<Self> {
        let app_version = AppVersion::global(cx).to_string();
        let release_channel = ReleaseChannel::global(cx);
        let os_name = Self::os_name();
        let system = System::new_with_specifics(
            RefreshKind::new().with_memory(MemoryRefreshKind::everything()),
        );
        let memory = system.total_memory();
        let architecture = env::consts::ARCH;
        let commit_sha = match release_channel {
            ReleaseChannel::Dev | ReleaseChannel::Nightly => {
                AppCommitSha::try_global(cx).map(|sha| sha.0.clone())
            }
            _ => None,
        };

        cx.background_executor().spawn(async move {
            let os_version = Self::os_version();
            SystemSpecs {
                app_version,
                release_channel: release_channel.display_name(),
                os_name,
                os_version,
                memory,
                architecture,
                commit_sha,
            }
        })
    }

    fn os_name() -> String {
        #[cfg(target_os = "macos")]
        {
            "macOS".to_string()
        }
        #[cfg(target_os = "linux")]
        {
            format!("Linux {}", gpui::guess_compositor())
        }

        #[cfg(target_os = "windows")]
        {
            "Windows".to_string()
        }
    }

    /// Note: This might do blocking IO! Only call from background threads
    fn os_version() -> String {
        #[cfg(target_os = "macos")]
        {
            use cocoa::base::nil;
            use cocoa::foundation::NSProcessInfo;

            unsafe {
                let process_info = cocoa::foundation::NSProcessInfo::processInfo(nil);
                let version = process_info.operatingSystemVersion();
                gpui::SemanticVersion::new(
                    version.majorVersion as usize,
                    version.minorVersion as usize,
                    version.patchVersion as usize,
                )
                .to_string()
            }
        }
        #[cfg(target_os = "linux")]
        {
            use std::path::Path;

            let content = if let Ok(file) = std::fs::read_to_string(&Path::new("/etc/os-release")) {
                file
            } else if let Ok(file) = std::fs::read_to_string(&Path::new("/usr/lib/os-release")) {
                file
            } else {
                log::error!("Failed to load /etc/os-release, /usr/lib/os-release");
                "".to_string()
            };
            let mut name = "unknown".to_string();
            let mut version = "unknown".to_string();

            for line in content.lines() {
                if line.starts_with("ID=") {
                    name = line.trim_start_matches("ID=").trim_matches('"').to_string();
                }
                if line.starts_with("VERSION_ID=") {
                    version = line
                        .trim_start_matches("VERSION_ID=")
                        .trim_matches('"')
                        .to_string();
                }
            }

            format!("{} {}", name, version)
        }

        #[cfg(target_os = "windows")]
        {
            let mut info = unsafe { std::mem::zeroed() };
            let status = unsafe { windows::Wdk::System::SystemServices::RtlGetVersion(&mut info) };
            if status.is_ok() {
                gpui::SemanticVersion::new(
                    info.dwMajorVersion as _,
                    info.dwMinorVersion as _,
                    info.dwBuildNumber as _,
                )
                .to_string()
            } else {
                "unknown".to_string()
            }
        }
    }

}

impl Display for SystemSpecs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let os_information = format!("OS: {} {}", self.os_name, self.os_version);
        let app_version_information = format!(
            "Zed: v{} ({})",
            self.app_version,
            match &self.commit_sha {
                Some(commit_sha) => format!("{} {}", self.release_channel, commit_sha),
                None => self.release_channel.to_string(),
            }
        );
        let system_specs = [
            app_version_information,
            os_information,
            format!("Memory: {}", human_bytes(self.memory as f64)),
            format!("Architecture: {}", self.architecture),
        ]
        .into_iter()
        .collect::<Vec<String>>()
        .join("\n");

        write!(f, "{system_specs}")
    }
}
