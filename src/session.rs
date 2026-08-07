use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{human_bytes, image_info, ObjectInfo};

const READY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MountedVolume {
    source: String,
    mount_point: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MountSession {
    version: u32,
    id: String,
    image: PathBuf,
    object: u32,
    server_pid: u32,
    listen_port: u16,
    nfs_mount: PathBuf,
    raw_file: PathBuf,
    #[serde(default)]
    nfs_mounted: bool,
    device: Option<String>,
    volumes: Vec<MountedVolume>,
    #[serde(default)]
    complete: bool,
    platform: String,
    created_unix_seconds: u64,
}

pub(crate) fn mount(image: &Path, requested_object: Option<u32>) -> Result<()> {
    ensure_supported_host()?;
    let image = image
        .canonicalize()
        .with_context(|| format!("resolve {}", image.display()))?;
    let info = image_info(&image)?;
    let object = select_object(&info.objects, requested_object)?;
    let id = new_session_id(object.id)?;
    let directory = sessions_root()?.join(&id);
    let nfs_mount = directory.join("nfs");
    let volumes_root = directory.join("volumes");
    create_private_directory(&directory)?;
    fs::create_dir_all(&nfs_mount)?;
    fs::create_dir_all(&volumes_root)?;

    let ready_file = directory.join("server.ready");
    let log_path = directory.join("server.log");
    let mut server = spawn_server(&image, object.id, &ready_file, &log_path)?;
    let listen_port = match wait_until_ready(&mut server, &ready_file) {
        Ok(port) => port,
        Err(error) => {
            stop_child(&mut server);
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
    };

    let raw_file = nfs_mount.join(format!("object-{}.raw", object.id));
    let mut session = MountSession {
        version: 1,
        id: id.clone(),
        image,
        object: object.id,
        server_pid: server.id(),
        listen_port,
        nfs_mount,
        raw_file,
        nfs_mounted: false,
        device: None,
        volumes: Vec::new(),
        complete: false,
        platform: std::env::consts::OS.to_owned(),
        created_unix_seconds: unix_seconds()?,
    };

    if let Err(error) = write_session(&directory, &session) {
        stop_child(&mut server);
        let _ = fs::remove_dir_all(&directory);
        return Err(error).context("persist initial mount session");
    }

    let result = (|| -> Result<()> {
        mount_nfs(&session)?;
        session.nfs_mounted = true;
        write_session(&directory, &session)?;
        attach_host_device(&mut session, &directory, &volumes_root)?;
        session.complete = true;
        write_session(&directory, &session)
    })();
    if let Err(error) = result {
        session.complete = false;
        let _ = write_session(&directory, &session);
        return match cleanup_resources(&directory, &mut session) {
            Ok(()) => {
                stop_child(&mut server);
                let _ = fs::remove_dir_all(&directory);
                Err(error).context("mount session failed and was rolled back")
            }
            Err(cleanup_error) => Err(error).context(format!(
                "mount session failed; cleanup also failed: {cleanup_error:#}; session {} was preserved for `rdrkit unmount {}`",
                session.id, session.id
            )),
        };
    }

    println!(
        "mounted session={} object={} size={} device={}",
        session.id,
        object.id,
        human_bytes(object.logical_size),
        session.device.as_deref().unwrap_or("none")
    );
    for volume in &session.volumes {
        println!(
            "volume={} mount={}",
            volume.source,
            volume.mount_point.display()
        );
    }
    if session.volumes.is_empty() {
        println!("no recognized filesystem was mounted; the disk remains attached read-only");
    }
    Ok(())
}

pub(crate) fn unmount(target: &str) -> Result<()> {
    ensure_supported_host()?;
    let directory = resolve_session(target)?;
    let mut session = read_session(&directory)?;

    session.complete = false;
    write_session(&directory, &session)?;
    cleanup_resources(&directory, &mut session)?;
    stop_server(&session)?;
    fs::remove_dir_all(&directory)
        .with_context(|| format!("remove session directory {}", directory.display()))?;
    println!(
        "unmounted session={} image={}",
        session.id,
        session.image.display()
    );
    Ok(())
}

pub(crate) fn status() -> Result<()> {
    let sessions = load_sessions()?;
    if sessions.is_empty() {
        println!("no rdrkit mount sessions");
        return Ok(());
    }
    for (_, session) in sessions {
        let state = if !server_is_running(&session) {
            "stale"
        } else if session.complete {
            "active"
        } else {
            "incomplete"
        };
        println!(
            "session={} state={} image={} object={} device={} volumes={}",
            session.id,
            state,
            session.image.display(),
            session.object,
            session.device.as_deref().unwrap_or("none"),
            session.volumes.len()
        );
    }
    Ok(())
}

fn ensure_supported_host() -> Result<()> {
    ensure!(
        cfg!(target_os = "macos") || cfg!(target_os = "linux"),
        "mount sessions are supported only on macOS and Linux"
    );
    Ok(())
}

fn select_object(objects: &[ObjectInfo], requested: Option<u32>) -> Result<ObjectInfo> {
    ensure!(!objects.is_empty(), "image contains no indexed objects");
    if let Some(id) = requested {
        return objects
            .iter()
            .find(|object| object.id == id)
            .cloned()
            .with_context(|| format!("object {id} was not found"));
    }
    if objects.len() == 1 {
        return Ok(objects[0].clone());
    }
    if !io::stdin().is_terminal() {
        bail!("image contains several objects; pass --object N");
    }

    eprintln!("Select an object to mount:");
    for object in objects {
        eprintln!("  {}: {}", object.id, human_bytes(object.logical_size));
    }
    eprint!("Object number: ");
    io::stderr().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let id = input
        .trim()
        .parse::<u32>()
        .context("invalid object number")?;
    objects
        .iter()
        .find(|object| object.id == id)
        .cloned()
        .with_context(|| format!("object {id} was not found"))
}

fn spawn_server(image: &Path, object: u32, ready_file: &Path, log_path: &Path) -> Result<Child> {
    let executable = std::env::current_exe().context("locate rdrkit executable")?;
    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(log_path)
        .with_context(|| format!("create {}", log_path.display()))?;
    let stderr = log.try_clone()?;
    Command::new("nohup")
        .arg(executable)
        .arg("serve")
        .arg(image)
        .arg("--object")
        .arg(object.to_string())
        .arg("--listen")
        .arg("127.0.0.1:0")
        .arg("--ready-file")
        .arg(ready_file)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("start rdrkit NFS server")
}

fn wait_until_ready(child: &mut Child, ready_file: &Path) -> Result<u16> {
    let started = std::time::Instant::now();
    while started.elapsed() < READY_TIMEOUT {
        if let Ok(value) = fs::read_to_string(ready_file) {
            let port = value.trim().parse::<u16>().context("invalid server port")?;
            ensure!(port > 0, "server selected an invalid port");
            return Ok(port);
        }
        if let Some(status) = child.try_wait()? {
            bail!("NFS server exited before becoming ready: {status}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("NFS server did not become ready within 30 seconds")
}

fn mount_nfs(session: &MountSession) -> Result<()> {
    let options = format!(
        "ro,nolocks,vers=3,tcp,rsize=131072,actimeo=1,port={0},mountport={0}",
        session.listen_port
    );
    #[cfg(target_os = "macos")]
    run_checked(
        Command::new("mount_nfs")
            .arg("-o")
            .arg(options)
            .arg("127.0.0.1:/")
            .arg(&session.nfs_mount),
        "mount localhost NFS export",
    )?;
    #[cfg(target_os = "linux")]
    run_checked(
        Command::new("sudo")
            .args(["mount", "-t", "nfs", "-o"])
            .arg(options)
            .arg("127.0.0.1:/")
            .arg(&session.nfs_mount),
        "mount localhost NFS export",
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn attach_host_device(
    session: &mut MountSession,
    directory: &Path,
    _volumes_root: &Path,
) -> Result<()> {
    let output = run_checked(
        Command::new("hdiutil")
            .args(["attach", "-readonly", "-nomount", "-imagekey"])
            .arg("diskimage-class=CRawDiskImage")
            .arg(&session.raw_file),
        "attach raw disk image",
    )?;
    let text = String::from_utf8(output.stdout).context("hdiutil returned non-UTF-8 output")?;
    let device = parse_macos_whole_disk(&text).context("hdiutil did not report a whole disk")?;
    session.device = Some(device.clone());
    write_session(directory, session)?;

    let _ = run_checked(
        Command::new("diskutil").args(["mountDisk", &device]),
        "mount recognized disk volumes",
    );
    session.volumes = mounted_macos_volumes(&device)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn mounted_macos_volumes(device: &str) -> Result<Vec<MountedVolume>> {
    let output = run_checked(&mut Command::new("mount"), "list mounted filesystems")?;
    let text = String::from_utf8(output.stdout).context("mount returned non-UTF-8 output")?;
    Ok(parse_macos_mounts(device, &text))
}

#[cfg(target_os = "linux")]
fn attach_host_device(
    session: &mut MountSession,
    directory: &Path,
    volumes_root: &Path,
) -> Result<()> {
    let output = run_checked(
        Command::new("sudo")
            .args(["losetup", "--find", "--show", "--read-only", "--partscan"])
            .arg(&session.raw_file),
        "attach read-only loop device",
    )?;
    let device = String::from_utf8(output.stdout)
        .context("losetup returned non-UTF-8 output")?
        .trim()
        .to_owned();
    ensure!(!device.is_empty(), "losetup did not report a loop device");
    session.device = Some(device.clone());
    write_session(directory, session)?;

    let candidates = wait_for_linux_filesystems(&device)?;
    ensure!(
        !candidates.is_empty(),
        "loop device contains no filesystems recognized by lsblk"
    );
    for (index, source) in candidates.into_iter().enumerate() {
        let mount_point = volumes_root.join(format!("volume-{}", index + 1));
        fs::create_dir_all(&mount_point)?;
        run_checked(
            Command::new("sudo")
                .args(["mount", "-o", "ro,nosuid,nodev,noexec"])
                .arg(&source)
                .arg(&mount_point),
            "mount read-only filesystem",
        )?;
        session.volumes.push(MountedVolume {
            source,
            mount_point,
        });
        write_session(directory, session)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct Lsblk {
    blockdevices: Vec<BlockDevice>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct BlockDevice {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    fstype: Option<String>,
    #[serde(default)]
    children: Vec<BlockDevice>,
}

#[cfg(target_os = "linux")]
fn collect_mountable_blocks(block: &BlockDevice, output: &mut Vec<String>) {
    if block.fstype.as_deref().is_some_and(is_mountable_fstype)
        && matches!(block.kind.as_str(), "loop" | "part")
    {
        output.push(block.path.clone());
    }
    for child in &block.children {
        collect_mountable_blocks(child, output);
    }
}

#[cfg(target_os = "linux")]
fn wait_for_linux_filesystems(device: &str) -> Result<Vec<String>> {
    let started = std::time::Instant::now();
    loop {
        let output = run_checked(
            Command::new("lsblk")
                .args(["--json", "--paths", "--output", "PATH,TYPE,FSTYPE,LABEL"])
                .arg(device),
            "inspect loop device filesystems",
        )?;
        let listing: Lsblk =
            serde_json::from_slice(&output.stdout).context("decode lsblk output")?;
        let mut candidates = Vec::new();
        for block in &listing.blockdevices {
            collect_mountable_blocks(block, &mut candidates);
        }
        if !candidates.is_empty() || started.elapsed() >= Duration::from_secs(2) {
            return Ok(candidates);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "linux")]
fn is_mountable_fstype(value: &str) -> bool {
    !value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "swap" | "crypto_luks" | "lvm2_member" | "linux_raid_member" | "zfs_member"
        )
}

#[cfg(target_os = "macos")]
fn detach_host_device(session: &mut MountSession, directory: &Path) -> Result<()> {
    if let Some(device) = &session.device {
        run_checked(
            Command::new("hdiutil").args(["detach", device]),
            "detach disk image",
        )?;
        session.device = None;
        session.volumes.clear();
        write_session(directory, session)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn detach_host_device(session: &mut MountSession, directory: &Path) -> Result<()> {
    while let Some(volume) = session.volumes.last().cloned() {
        run_checked(
            Command::new("sudo").arg("umount").arg(&volume.mount_point),
            "unmount filesystem",
        )?;
        session.volumes.pop();
        write_session(directory, session)?;
    }
    if let Some(device) = &session.device {
        run_checked(
            Command::new("sudo").args(["losetup", "--detach", device]),
            "detach loop device",
        )?;
        session.device = None;
        write_session(directory, session)?;
    }
    Ok(())
}

fn unmount_nfs(session: &mut MountSession, directory: &Path) -> Result<()> {
    if !session.nfs_mounted {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    run_checked(
        Command::new("umount").arg(&session.nfs_mount),
        "unmount localhost NFS export",
    )?;
    #[cfg(target_os = "linux")]
    run_checked(
        Command::new("sudo").arg("umount").arg(&session.nfs_mount),
        "unmount localhost NFS export",
    )?;
    session.nfs_mounted = false;
    write_session(directory, session)?;
    Ok(())
}

fn cleanup_resources(directory: &Path, session: &mut MountSession) -> Result<()> {
    detach_host_device(session, directory)?;
    unmount_nfs(session, directory)
}

fn run_checked(command: &mut Command, action: &str) -> Result<Output> {
    let output = command
        .output()
        .with_context(|| format!("{action}: execute command"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{action}: {}", stderr.trim());
    }
    Ok(output)
}

fn write_session(directory: &Path, session: &MountSession) -> Result<()> {
    let target = directory.join("session.json");
    let temporary = directory.join("session.json.tmp");
    let bytes = serde_json::to_vec_pretty(session)?;
    fs::write(&temporary, bytes)?;
    set_private_permissions(&temporary, 0o600)?;
    fs::rename(&temporary, &target)?;
    Ok(())
}

fn create_private_directory(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)
        .with_context(|| format!("create session directory {}", directory.display()))?;
    set_private_permissions(directory, 0o700)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn read_session(directory: &Path) -> Result<MountSession> {
    let path = directory.join("session.json");
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("decode {}", path.display()))
}

fn load_sessions() -> Result<Vec<(PathBuf, MountSession)>> {
    let root = sessions_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let directory = entry.path();
        match read_session(&directory) {
            Ok(session) => sessions.push((directory, session)),
            Err(error) => eprintln!("skip invalid session {}: {error:#}", directory.display()),
        }
    }
    sessions.sort_by(|left, right| left.1.id.cmp(&right.1.id));
    Ok(sessions)
}

fn resolve_session(target: &str) -> Result<PathBuf> {
    let sessions = load_sessions()?;
    if let Some((directory, _)) = sessions.iter().find(|(_, session)| session.id == target) {
        return Ok(directory.clone());
    }
    let target_path = Path::new(target).canonicalize().ok();
    let matches: Vec<_> = sessions
        .iter()
        .filter(|(_, session)| target_path.as_ref() == Some(&session.image))
        .collect();
    ensure!(
        matches.len() <= 1,
        "several sessions use this image; pass a session id"
    );
    matches
        .first()
        .map(|(directory, _)| (*directory).clone())
        .with_context(|| format!("mount session not found: {target}"))
}

fn sessions_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("RDRKIT_STATE_DIR") {
        return Ok(PathBuf::from(path).join("sessions"));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("rdrkit")
            .join("sessions"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
            return Ok(PathBuf::from(path).join("rdrkit").join("sessions"));
        }
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("rdrkit")
            .join("sessions"))
    }
}

fn new_session_id(object: u32) -> Result<String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis();
    Ok(format!("{millis}-{}-{object}", std::process::id()))
}

fn unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn stop_server(session: &MountSession) -> Result<()> {
    if !server_is_running(session) {
        return Ok(());
    }
    let output = Command::new("kill")
        .args(["-TERM", &session.server_pid.to_string()])
        .output()
        .context("stop rdrkit NFS server")?;
    if !output.status.success() && server_is_running(session) {
        bail!(
            "stop rdrkit NFS server: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn server_is_running(session: &MountSession) -> bool {
    let output = Command::new("ps")
        .args(["-p", &session.server_pid.to_string(), "-o", "command="])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    server_command_matches(session, &String::from_utf8_lossy(&output.stdout))
}

fn server_command_matches(session: &MountSession, command: &str) -> bool {
    command.contains("rdrkit")
        && command.contains(" serve ")
        && command.contains(session.image.to_string_lossy().as_ref())
}

#[cfg(target_os = "macos")]
fn parse_macos_whole_disk(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let device = line.split_whitespace().next()?;
        let suffix = device.strip_prefix("/dev/disk")?;
        suffix
            .chars()
            .all(|character| character.is_ascii_digit())
            .then(|| device.to_owned())
    })
}

#[cfg(target_os = "macos")]
fn parse_macos_mounts(device: &str, output: &str) -> Vec<MountedVolume> {
    output
        .lines()
        .filter_map(|line| {
            let (source, rest) = line.split_once(" on ")?;
            let suffix = source.strip_prefix(device)?;
            let is_slice = suffix.strip_prefix('s').is_some_and(|value| {
                !value.is_empty() && value.chars().all(|item| item.is_ascii_digit())
            });
            if !suffix.is_empty() && !is_slice {
                return None;
            }
            let mount_point = rest.split_once(" (").map_or(rest, |(path, _)| path);
            Some(MountedVolume {
                source: source.to_owned(),
                mount_point: PathBuf::from(mount_point),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(id: u32, size: u64) -> ObjectInfo {
        ObjectInfo {
            id,
            logical_size: size,
            chunks: 1,
            chunk_size: size as u32,
        }
    }

    #[test]
    fn selects_requested_or_only_object() -> Result<()> {
        let objects = vec![object(3, 512), object(7, 1_024)];
        assert_eq!(select_object(&objects, Some(7))?.id, 7);
        assert_eq!(select_object(&objects[..1], None)?.id, 3);
        assert!(select_object(&objects, Some(9)).is_err());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn parses_whole_disk_from_hdiutil_output() {
        let output = "/dev/disk9\tGUID_partition_scheme\n/dev/disk9s1\tEFI\n";
        assert_eq!(
            parse_macos_whole_disk(output).as_deref(),
            Some("/dev/disk9")
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn finds_only_slices_belonging_to_macos_disk() {
        let output = concat!(
            "/dev/disk9s1 on /Volumes/Data (ntfs, local, read-only)\n",
            "/dev/disk90s1 on /Volumes/Other (apfs, local)\n",
            "/dev/disk9 on /Volumes/Whole (ntfs, local, read-only)\n"
        );
        let volumes = parse_macos_mounts("/dev/disk9", output);
        assert_eq!(volumes.len(), 2);
        assert_eq!(volumes[0].source, "/dev/disk9s1");
        assert_eq!(volumes[0].mount_point, Path::new("/Volumes/Data"));
        assert_eq!(volumes[1].source, "/dev/disk9");
        assert_eq!(volumes[1].mount_point, Path::new("/Volumes/Whole"));
    }

    #[test]
    fn session_state_round_trips() -> Result<()> {
        let session = MountSession {
            version: 1,
            id: "session-1".to_owned(),
            image: PathBuf::from("/images/backup.rdr"),
            object: 3,
            server_pid: 42,
            listen_port: 12_345,
            nfs_mount: PathBuf::from("/state/nfs"),
            raw_file: PathBuf::from("/state/nfs/object-3.raw"),
            nfs_mounted: true,
            device: Some("/dev/disk9".to_owned()),
            volumes: vec![MountedVolume {
                source: "/dev/disk9s1".to_owned(),
                mount_point: PathBuf::from("/Volumes/Data"),
            }],
            complete: true,
            platform: "macos".to_owned(),
            created_unix_seconds: 123,
        };
        let encoded = serde_json::to_vec(&session)?;
        let decoded: MountSession = serde_json::from_slice(&encoded)?;
        assert_eq!(decoded.id, session.id);
        assert_eq!(decoded.image, session.image);
        assert_eq!(decoded.volumes[0].mount_point, Path::new("/Volumes/Data"));
        assert!(decoded.nfs_mounted);
        assert!(decoded.complete);
        Ok(())
    }

    #[test]
    fn older_session_state_defaults_lifecycle_flags() -> Result<()> {
        let encoded = r#"{
            "version": 1,
            "id": "session-1",
            "image": "/images/backup.rdr",
            "object": 3,
            "server_pid": 42,
            "listen_port": 12345,
            "nfs_mount": "/state/nfs",
            "raw_file": "/state/nfs/object-3.raw",
            "device": null,
            "volumes": [],
            "platform": "macos",
            "created_unix_seconds": 123
        }"#;
        let session: MountSession = serde_json::from_str(encoded)?;
        assert!(!session.nfs_mounted);
        assert!(!session.complete);
        Ok(())
    }

    #[test]
    fn matches_only_the_recorded_server_process() {
        let session = MountSession {
            version: 1,
            id: "session-1".to_owned(),
            image: PathBuf::from("/images/backup with spaces.rdr"),
            object: 3,
            server_pid: 42,
            listen_port: 12_345,
            nfs_mount: PathBuf::from("/state/nfs"),
            raw_file: PathBuf::from("/state/nfs/object-3.raw"),
            nfs_mounted: true,
            device: None,
            volumes: Vec::new(),
            complete: true,
            platform: "macos".to_owned(),
            created_unix_seconds: 123,
        };
        assert!(server_command_matches(
            &session,
            "/usr/local/bin/rdrkit serve '/images/backup with spaces.rdr' --object 3"
        ));
        assert!(!server_command_matches(
            &session,
            "/usr/local/bin/rdrkit list '/images/backup with spaces.rdr'"
        ));
        assert!(!server_command_matches(
            &session,
            "/usr/local/bin/rdrkit serve /images/other.rdr --object 3"
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn finds_mountable_loop_filesystems() -> Result<()> {
        let listing: Lsblk = serde_json::from_str(
            r#"{"blockdevices":[{"path":"/dev/loop0","type":"loop","fstype":null,"label":null,"children":[{"path":"/dev/loop0p1","type":"part","fstype":"ntfs","label":"Data"}]}]}"#,
        )?;
        let mut found = Vec::new();
        collect_mountable_blocks(&listing.blockdevices[0], &mut found);
        assert_eq!(found, ["/dev/loop0p1"]);
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn rejects_linux_container_filesystems() {
        for value in [
            "swap",
            "crypto_LUKS",
            "LVM2_member",
            "linux_raid_member",
            "zfs_member",
        ] {
            assert!(
                !is_mountable_fstype(value),
                "unexpected mountable type: {value}"
            );
        }
        assert!(is_mountable_fstype("ntfs"));
        assert!(is_mountable_fstype("ext4"));
    }
}
