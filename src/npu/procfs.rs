//! `/proc` enrichment for NPU processes.
//!
//! `npu-smi` reports *host* PIDs. On the host — or in a `--pid=host`
//! container — each PID has a `/proc` entry whose `cmdline`, `status` and
//! `cgroup` files reveal who owns the card memory (full command line,
//! user, RSS, and which container it belongs to). Inside a namespaced
//! container the host PID has no `/proc` entry; `npu-smi` 26.x then
//! offers a second path: its `Process id in container` column resolves
//! `/proc` exactly when the process shares our container. Everything
//! else only records `visible: false` so the UI can say "host-side"
//! instead of guessing.
//!
//! The root paths are injectable so every parser is unit-testable on CI
//! machines without an NPU, containers, or even real PIDs.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::model::{NpuProcess, ProcExtra};
use super::smi;

/// `docker ps` is best-effort name decoration; keep it cheap.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(2);

/// Reads `/proc` and `/etc/passwd`. Paths are injected at construction;
/// production code uses [`Procfs::system`].
pub struct Procfs {
    proc_dir: PathBuf,
    /// uid → login name, parsed once so per-PID probes never re-read it.
    passwd: HashMap<u32, String>,
}

impl Procfs {
    /// The real system view.
    pub fn system() -> Self {
        Self::new(Path::new("/proc"), Path::new("/etc/passwd"))
    }

    /// Rooted at arbitrary paths, for tests.
    pub fn new(proc_dir: &Path, passwd_file: &Path) -> Self {
        Self {
            proc_dir: proc_dir.to_path_buf(),
            passwd: parse_passwd(&fs::read_to_string(passwd_file).unwrap_or_default()),
        }
    }

    /// Reads host-side facts for each unique process, keyed by host PID.
    pub fn enrich(
        &self,
        procs: &[NpuProcess],
        containers: Option<&HashMap<String, String>>,
    ) -> HashMap<u32, ProcExtra> {
        let mut out = HashMap::new();
        for proc in procs {
            out.entry(proc.pid)
                .or_insert_with(|| self.probe(proc, containers));
        }
        out
    }

    /// Picks the resolvable `/proc` entry: the host PID first, then the
    /// container PID — which lands only inside the process's own
    /// container. Returns an invisible `ProcExtra` when neither exists.
    fn probe(&self, proc: &NpuProcess, containers: Option<&HashMap<String, String>>) -> ProcExtra {
        let host_dir = self.proc_dir.join(proc.pid.to_string());
        if host_dir.is_dir() {
            return self.read_pid(proc.pid, host_dir, containers);
        }
        if let Some(container_pid) = proc.container_pid {
            let dir = self.proc_dir.join(container_pid.to_string());
            if dir.is_dir() {
                let mut extra = self.read_pid(container_pid, dir, containers);
                extra.via_container_pid = true;
                return extra;
            }
        }
        ProcExtra::default()
    }

    fn read_pid(
        &self,
        pid: u32,
        dir: PathBuf,
        containers: Option<&HashMap<String, String>>,
    ) -> ProcExtra {
        let cmdline = fs::read(dir.join("cmdline")).ok().and_then(|raw| {
            let joined = raw
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        });
        let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
        let user = status_uid(&status).map(|uid| {
            self.passwd
                .get(&uid)
                .cloned()
                .unwrap_or_else(|| uid.to_string())
        });
        let rss_mb = status_rss_mb(&status);
        let container_id = fs::read_to_string(dir.join("cgroup"))
            .ok()
            .and_then(|text| container_id(&text));
        let container = container_id.as_ref().map(|id| {
            containers
                .and_then(|map| container_name(map, id))
                .unwrap_or_else(|| short_id(id))
        });
        ProcExtra {
            visible: true,
            local_pid: Some(pid),
            via_container_pid: false,
            user,
            rss_mb,
            cmdline,
            container_id,
            container,
        }
    }
}

/// `name:x:uid:...` lines → uid → name.
fn parse_passwd(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?.to_string();
            fields.next()?;
            let uid: u32 = fields.next()?.parse().ok()?;
            Some((uid, name))
        })
        .collect()
}

/// Real uid (first of the four `Uid:` fields) from `/proc/<pid>/status`.
fn status_uid(status: &str) -> Option<u32> {
    let line = status.lines().find(|line| line.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// `VmRSS:` value converted to MiB.
fn status_rss_mb(status: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// Extracts a container runtime id from `/proc/<pid>/cgroup`.
///
/// Handles the shapes seen on docker (`docker/<64hex>` or
/// `docker-<64>.scope`), containerd/cri-o (`cri-containerd-` / `crio-`
/// scopes) and kubepods paths. The *last* hex token of at least 12 chars
/// wins: in a kubepods path the pod UID parts appear earlier while the
/// innermost scope carries the real container id.
pub fn container_id(cgroup: &str) -> Option<String> {
    let mut found = None;
    for line in cgroup.lines() {
        for token in line.split(['/', '.', '-', ':']) {
            if token.len() >= 12
                && token.len() <= 64
                && token.chars().all(|c| c.is_ascii_hexdigit())
            {
                found = Some(token.to_string());
            }
        }
    }
    found
}

/// First 12 chars of a runtime id, the same form `docker ps` prints.
fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// Finds the container name for a runtime id. `docker ps` reports the
/// 12-char short id while cgroups carry the full 64-char one, so compare
/// by prefix either way.
fn container_name(map: &HashMap<String, String>, id: &str) -> Option<String> {
    map.iter()
        .find(|(short, _)| id.starts_with(short.as_str()) || short.starts_with(id))
        .map(|(_, name)| name.clone())
}

/// `docker ps` id → name map. Empty when docker is absent, slow, or the
/// daemon is unreachable — enrichment still falls back to short ids.
pub fn docker_names() -> HashMap<String, String> {
    let mut cmd = Command::new("docker");
    cmd.args(["ps", "--format", "{{.ID}} {{.Names}}"]);
    let Ok(text) = smi::run_deadline(&mut cmd, DOCKER_TIMEOUT) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let (id, name) = line.split_once(' ')?;
            Some((id.trim().to_string(), name.trim().to_string()))
        })
        .filter(|(id, name)| !id.is_empty() && !name.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX64: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";

    #[test]
    fn passwd_maps_uid_to_name() {
        let map = parse_passwd("root:x:0:0:root:/root:/bin/sh\nu:x:1000:1000::/home/u:/bin/bash\n");
        assert_eq!(map[&0], "root");
        assert_eq!(map[&1000], "u");
        assert!(!map.contains_key(&1));
    }

    #[test]
    fn status_reads_uid_and_rss() {
        let status = "Name:\tpython3\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t   20480 kB\n";
        assert_eq!(status_uid(status), Some(1000));
        assert_eq!(status_rss_mb(status), Some(20));
    }

    #[test]
    fn container_id_reads_docker_cgroup_v2() {
        let text = format!("0::/docker/{HEX64}\n");
        assert_eq!(container_id(&text).as_deref(), Some(HEX64));
    }

    #[test]
    fn container_id_reads_systemd_scope() {
        let text = format!("1:name=systemd:/system.slice/docker-{HEX64}.scope\n");
        assert_eq!(container_id(&text).as_deref(), Some(HEX64));
    }

    #[test]
    fn container_id_prefers_innermost_scope_over_pod_uid() {
        let pod_uid = "0123456789ab"; // 12 hex chars — matches, but is not last
        let text = format!(
            "0::/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod{pod_uid}.slice/cri-containerd-{HEX64}.scope\n"
        );
        assert_eq!(container_id(&text).as_deref(), Some(HEX64));
    }

    #[test]
    fn container_id_returns_none_on_the_host() {
        assert_eq!(
            container_id("0::/user.slice/user-0.slice/session-1.scope\n"),
            None
        );
        assert_eq!(container_id("0::/\n"), None);
    }

    #[test]
    fn container_name_matches_short_id_by_prefix() {
        let mut map = HashMap::new();
        map.insert("a1b2c3d4e5f6".to_string(), "pytorch".to_string());
        assert_eq!(container_name(&map, HEX64).as_deref(), Some("pytorch"));
        assert_eq!(container_name(&map, "ffffffffffff").as_deref(), None);
    }

    /// Writes a fake `/proc/<pid>` tree under a unique temp dir. The
    /// counter keeps parallel tests in this process from wiping each
    /// other's fixture.
    fn fake_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "utop-procfs-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn proc(pid: u32, container_pid: Option<u32>) -> NpuProcess {
        NpuProcess {
            pid,
            container_pid,
            ..Default::default()
        }
    }

    #[test]
    fn enrich_reads_cmdline_status_and_cgroup() {
        let root = fake_root();
        let proc_dir = root.join("proc");
        let pid_dir = proc_dir.join("4242");
        fs::create_dir_all(&pid_dir).unwrap();
        fs::write(pid_dir.join("cmdline"), b"python3\0-m\0vllm.entrypoints\0").unwrap();
        fs::write(
            pid_dir.join("status"),
            "Uid:\t0\t0\t0\t0\nVmRSS:\t1048576 kB\n",
        )
        .unwrap();
        fs::write(pid_dir.join("cgroup"), format!("0::/docker/{HEX64}\n")).unwrap();
        fs::write(root.join("passwd"), "root:x:0:0:root:/root:/bin/sh\n").unwrap();

        let procfs = Procfs::new(&proc_dir, &root.join("passwd"));
        let mut docker = HashMap::new();
        docker.insert("a1b2c3d4e5f6".to_string(), "pytorch".to_string());
        // An unknown PID is reported invisible, not skipped.
        let extras = procfs.enrich(&[proc(4242, None), proc(9999, None)], Some(&docker));
        let extra = &extras[&4242];
        assert!(extra.visible);
        assert_eq!(extra.local_pid, Some(4242));
        assert_eq!(extra.user.as_deref(), Some("root"));
        assert_eq!(extra.rss_mb, Some(1024));
        assert_eq!(
            extra.cmdline.as_deref(),
            Some("python3 -m vllm.entrypoints")
        );
        assert_eq!(extra.container.as_deref(), Some("pytorch"));
        assert!(!extras[&9999].visible);
        assert_eq!(extras[&9999].ctn_label(), "?");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enrich_without_docker_falls_back_to_short_id() {
        let root = fake_root();
        let proc_dir = root.join("proc");
        let pid_dir = proc_dir.join("7");
        fs::create_dir_all(&pid_dir).unwrap();
        fs::write(
            pid_dir.join("cgroup"),
            format!("0::/system.slice/docker-{HEX64}.scope\n"),
        )
        .unwrap();
        let procfs = Procfs::new(&proc_dir, &root.join("passwd"));
        let extras = procfs.enrich(&[proc(7, None)], None);
        assert_eq!(extras[&7].container.as_deref(), Some("a1b2c3d4e5f6"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enrich_falls_back_to_the_container_pid() {
        let root = fake_root();
        let proc_dir = root.join("proc");
        // Only the in-container PID exists — the host-side 26.x view.
        let dir = proc_dir.join("88");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cmdline"), b"python3\0-m\0vllm\0").unwrap();
        fs::write(dir.join("status"), "Uid:\t0\t0\t0\t0\nVmRSS:\t4096 kB\n").unwrap();
        // Inside a container the cgroup path hides the host container id.
        fs::write(dir.join("cgroup"), "0::/\n").unwrap();
        fs::write(root.join("passwd"), "root:x:0:0:root:/root:/bin/sh\n").unwrap();

        let procfs = Procfs::new(&proc_dir, &root.join("passwd"));
        let extras = procfs.enrich(&[proc(4000000, Some(88))], None);
        let extra = &extras[&4000000];
        assert!(extra.visible);
        assert!(extra.via_container_pid);
        assert_eq!(extra.local_pid, Some(88));
        assert_eq!(extra.cmdline.as_deref(), Some("python3 -m vllm"));
        // Container attribution is unresolvable but clearly "this one".
        assert_eq!(extra.ctn_label(), "local");
        // A foreign container's pid resolves neither way.
        let extras = procfs.enrich(&[proc(5000000, Some(9999))], None);
        assert_eq!(extras[&5000000].ctn_label(), "?");

        let _ = fs::remove_dir_all(&root);
    }
}
