use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAGIC: u32 = 0xd754_da33;
const FILE_HEADER_SIZE: u64 = 52;
const RAW_DATA_FLAGS: u32 = 0x1800_0020;
const RAW_CHUNK_INDEX_FLAGS: u32 = 0x0800_0090;
const ARCHIVE_DIRECTORY_FLAGS: u32 = 0x0000_0008;
const DIRECTORY_POINTER_FLAGS: u32 = 0x0000_0003;

#[test]
fn released_binary_lists_and_serves_synthetic_image() -> Result<(), Box<dyn std::error::Error>> {
    let directory = temporary_directory("serve lifecycle with spaces")?;
    let image = directory.join("synthetic image.rdr");
    let ready = directory.join("server.ready");
    write_synthetic_image(&image)?;

    let list = Command::new(env!("CARGO_BIN_EXE_rdrkit"))
        .arg("list")
        .arg(&image)
        .output()?;
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_output = String::from_utf8(list.stdout)?;
    assert!(list_output.contains("object=0 size=512 B chunks=1 chunk_size=512 B"));

    let mut server = Command::new(env!("CARGO_BIN_EXE_rdrkit"))
        .arg("serve")
        .arg(&image)
        .args(["--object", "0", "--listen", "127.0.0.1:0", "--ready-file"])
        .arg(&ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let port = wait_for_ready_port(&mut server, &ready)?;
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let connection = TcpStream::connect_timeout(&address.into(), Duration::from_secs(2))?;
        drop(connection);
        assert!(
            server.try_wait()?.is_none(),
            "server exited after accepting a connection"
        );
        Ok(())
    })();

    let _ = server.kill();
    let _ = server.wait();
    fs::remove_dir_all(directory)?;
    result
}

#[test]
fn invalid_image_exits_without_publishing_readiness() -> Result<(), Box<dyn std::error::Error>> {
    let directory = temporary_directory("invalid-image")?;
    let image = directory.join("invalid.rdr");
    let ready = directory.join("server.ready");
    fs::write(&image, b"not an rdr image")?;

    let output = Command::new(env!("CARGO_BIN_EXE_rdrkit"))
        .arg("serve")
        .arg(&image)
        .args(["--object", "0", "--listen", "127.0.0.1:0", "--ready-file"])
        .arg(&ready)
        .output()?;

    assert!(!output.status.success());
    assert!(!ready.exists());
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn managed_mount_status_and_unmount_are_recoverable() -> Result<(), Box<dyn std::error::Error>> {
    let directory = temporary_directory("managed-session")?;
    let image = directory.join("managed image.rdr");
    let state = directory.join("state");
    let fake_bin = directory.join("bin");
    let fail_once = directory.join("fail-once");
    fs::create_dir(&fake_bin)?;
    fs::write(&fail_once, b"fail the first detach")?;
    write_synthetic_image(&image)?;
    let canonical_image = image.canonicalize()?;
    write_fake_host_commands(&fake_bin)?;

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let configure = |command: &mut Command| {
        command
            .env("PATH", &path)
            .env("RDRKIT_STATE_DIR", &state)
            .env("RDRKIT_TEST_IMAGE", &canonical_image)
            .env("RDRKIT_TEST_FAIL_ONCE", &fail_once);
    };

    let mut mount = Command::new(env!("CARGO_BIN_EXE_rdrkit"));
    mount.args([
        "mount",
        image.to_str().ok_or("non-UTF-8 test path")?,
        "--object",
        "0",
    ]);
    configure(&mut mount);
    let mounted = mount.output()?;
    assert!(
        mounted.status.success(),
        "{}",
        String::from_utf8_lossy(&mounted.stderr)
    );
    let mounted_output = String::from_utf8(mounted.stdout)?;
    let session_id = parse_session_id(&mounted_output).ok_or("missing session id")?;
    let session_directory = state.join("sessions").join(&session_id);
    let session_file = session_directory.join("session.json");
    let session: serde_json::Value = serde_json::from_slice(&fs::read(&session_file)?)?;
    let server_pid = session["server_pid"].as_u64().ok_or("missing server pid")? as u32;
    let mut server_guard = ProcessGuard::new(server_pid);
    assert_eq!(session["complete"], true);
    assert_eq!(session["nfs_mounted"], true);

    let mut status = Command::new(env!("CARGO_BIN_EXE_rdrkit"));
    status.arg("status");
    configure(&mut status);
    let status_output = status.output()?;
    assert!(status_output.status.success());
    let status_text = String::from_utf8(status_output.stdout)?;
    assert!(status_text.contains("state=active"), "{status_text}");

    let mut first_unmount = Command::new(env!("CARGO_BIN_EXE_rdrkit"));
    first_unmount.args(["unmount", &session_id]);
    configure(&mut first_unmount);
    let first_result = first_unmount.output()?;
    assert!(!first_result.status.success());
    assert!(session_file.exists());
    let interrupted: serde_json::Value = serde_json::from_slice(&fs::read(&session_file)?)?;
    assert_eq!(interrupted["complete"], false);

    let mut retry = Command::new(env!("CARGO_BIN_EXE_rdrkit"));
    retry.args(["unmount", &session_id]);
    configure(&mut retry);
    let retry_result = retry.output()?;
    assert!(
        retry_result.status.success(),
        "{}",
        String::from_utf8_lossy(&retry_result.stderr)
    );
    assert!(!session_directory.exists());
    wait_for_process_exit(server_pid)?;
    server_guard.disarm();

    fs::remove_dir_all(directory)?;
    Ok(())
}

struct ProcessGuard {
    pid: u32,
    active: bool,
}

impl ProcessGuard {
    fn new(pid: u32) -> Self {
        Self { pid, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &self.pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn parse_session_id(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find_map(|item| item.strip_prefix("session=").map(str::to_owned))
}

fn wait_for_process_exit(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        let status = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("server process {pid} did not exit").into())
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, body)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn write_fake_host_commands(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    write_executable(&directory.join("mount_nfs"), "#!/bin/sh\nexit 0\n")?;
    write_executable(
        &directory.join("hdiutil"),
        r#"#!/bin/sh
if [ "$1" = "attach" ]; then
  printf '/dev/disk99\tGUID_partition_scheme\n/dev/disk99s1\tMicrosoft Basic Data\n'
  exit 0
fi
if [ -f "$RDRKIT_TEST_FAIL_ONCE" ]; then
  rm "$RDRKIT_TEST_FAIL_ONCE"
  printf 'injected detach failure\n' >&2
  exit 1
fi
exit 0
"#,
    )?;
    write_executable(&directory.join("diskutil"), "#!/bin/sh\nexit 0\n")?;
    write_executable(
        &directory.join("mount"),
        "#!/bin/sh\nprintf '/dev/disk99s1 on /Volumes/Test (ntfs, local, read-only)\\n'\n",
    )?;
    write_executable(&directory.join("umount"), "#!/bin/sh\nexit 0\n")?;
    write_common_fake_commands(directory)
}

#[cfg(target_os = "linux")]
fn write_fake_host_commands(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    write_executable(
        &directory.join("sudo"),
        r#"#!/bin/sh
if [ "$1" = "losetup" ] && [ "$2" = "--find" ]; then
  printf '/dev/loop99\n'
  exit 0
fi
if [ "$1" = "umount" ] && [ -f "$RDRKIT_TEST_FAIL_ONCE" ]; then
  rm "$RDRKIT_TEST_FAIL_ONCE"
  printf 'injected unmount failure\n' >&2
  exit 1
fi
exit 0
"#,
    )?;
    write_executable(
        &directory.join("lsblk"),
        r#"#!/bin/sh
printf '{"blockdevices":[{"path":"/dev/loop99","type":"loop","fstype":null,"children":[{"path":"/dev/loop99p1","type":"part","fstype":"ext4"}]}]}\n'
"#,
    )?;
    write_common_fake_commands(directory)
}

fn write_common_fake_commands(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    write_executable(
        &directory.join("ps"),
        "#!/bin/sh\nprintf 'rdrkit serve %s --object 0\\n' \"$RDRKIT_TEST_IMAGE\"\n",
    )?;
    write_executable(
        &directory.join("kill"),
        "#!/bin/sh\nexec /bin/kill \"$@\"\n",
    )?;
    Ok(())
}

fn wait_for_ready_port(
    server: &mut std::process::Child,
    ready: &Path,
) -> Result<u16, Box<dyn std::error::Error>> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if let Ok(value) = fs::read_to_string(ready) {
            return Ok(value.trim().parse()?);
        }
        if let Some(status) = server.try_wait()? {
            return Err(format!("server exited before readiness: {status}").into());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err("server readiness timed out".into())
}

fn write_synthetic_image(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let file_size = 4_096_u64;
    let logical_size = 512_u64;
    let data_offset = FILE_HEADER_SIZE;
    let data_length = 36_u32 + logical_size as u32;
    let index_offset = 1_024_u64;
    let index_length = 60_u32;
    let directory_offset = 2_048_u64;
    let directory_length = 32_u32;
    let pointer_offset = file_size - 24;

    let mut file = File::create(path)?;
    file.set_len(file_size)?;

    let mut header = [0_u8; FILE_HEADER_SIZE as usize];
    header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    header[4..8].copy_from_slice(&(FILE_HEADER_SIZE as u32).to_le_bytes());
    header[44..52].copy_from_slice(&file_size.to_le_bytes());
    file.write_all(&header)?;

    let mut data = vec![0_u8; data_length as usize];
    data[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    data[4..8].copy_from_slice(&data_length.to_le_bytes());
    data[8..12].copy_from_slice(&RAW_DATA_FLAGS.to_le_bytes());
    data[20..28].copy_from_slice(&0_u64.to_le_bytes());
    data[28..32].copy_from_slice(&(logical_size as u32).to_le_bytes());
    data[36..].fill(0xa5);
    file.seek(SeekFrom::Start(data_offset))?;
    file.write_all(&data)?;

    let mut index = vec![0_u8; index_length as usize];
    index[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    index[4..8].copy_from_slice(&index_length.to_le_bytes());
    index[8..12].copy_from_slice(&RAW_CHUNK_INDEX_FLAGS.to_le_bytes());
    index[12..16].copy_from_slice(&0_u32.to_le_bytes());
    index[20..28].copy_from_slice(&logical_size.to_le_bytes());
    index[36..44].copy_from_slice(&logical_size.to_le_bytes());
    index[44..48].copy_from_slice(&1_u32.to_le_bytes());
    index[48..56].copy_from_slice(&(data_offset - FILE_HEADER_SIZE).to_le_bytes());
    index[56..60].copy_from_slice(&data_length.to_le_bytes());
    file.seek(SeekFrom::Start(index_offset))?;
    file.write_all(&index)?;

    let mut directory = vec![0_u8; directory_length as usize];
    directory[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    directory[4..8].copy_from_slice(&directory_length.to_le_bytes());
    directory[8..12].copy_from_slice(&ARCHIVE_DIRECTORY_FLAGS.to_le_bytes());
    directory[12..20].copy_from_slice(&(index_offset - FILE_HEADER_SIZE).to_le_bytes());
    directory[20..24].copy_from_slice(&index_length.to_le_bytes());
    directory[24..28].copy_from_slice(&0_u32.to_le_bytes());
    directory[28..32].copy_from_slice(&0x90_u32.to_le_bytes());
    file.seek(SeekFrom::Start(directory_offset))?;
    file.write_all(&directory)?;

    let mut pointer = [0_u8; 24];
    pointer[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    pointer[4..8].copy_from_slice(&24_u32.to_le_bytes());
    pointer[8..12].copy_from_slice(&DIRECTORY_POINTER_FLAGS.to_le_bytes());
    pointer[12..20].copy_from_slice(&(directory_offset - FILE_HEADER_SIZE).to_le_bytes());
    pointer[20..24].copy_from_slice(&directory_length.to_le_bytes());
    file.seek(SeekFrom::Start(pointer_offset))?;
    file.write_all(&pointer)?;
    file.sync_all()?;
    Ok(())
}

fn temporary_directory(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = std::env::temp_dir().join(format!("rdrkit-{label}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path)?;
    Ok(path)
}
