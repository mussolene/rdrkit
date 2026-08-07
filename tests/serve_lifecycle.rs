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
    let directory = temporary_directory("serve-lifecycle")?;
    let image = directory.join("synthetic.rdr");
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
