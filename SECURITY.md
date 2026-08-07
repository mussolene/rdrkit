# Security policy

## Supported versions

Security fixes are provided for the latest released minor version.

## Reporting a vulnerability

Do not open a public issue for a vulnerability. Use GitHub's **Report a
vulnerability** button in the repository Security tab to submit a private
security advisory.

Include the affected version, a minimal reproducer, expected impact, and any
suggested mitigation. Please avoid attaching disk images that contain personal
or confidential data.

## Safety model

`rdrkit` is read-only with respect to the source RDR image. Mounted filesystems
must also be attached read-only. Treat every image as untrusted input and run
the tool with the minimum privileges required by the host operating system.

Read-only access prevents `rdrkit` and the mounted filesystem from intentionally
modifying the source. It does not protect the host from vulnerabilities in
filesystem parsers. A malicious NTFS, FAT, APFS, ext, or other filesystem may be
processed by privileged host code after the raw device is attached. Analyze
hostile or unknown images in an isolated, fully updated virtual machine.

The managed Linux workflow runs the parser and localhost NFS server as the
calling user. It invokes `sudo` only for `mount`, `umount`, and `losetup`.
The macOS workflow uses the system `mount_nfs`, `hdiutil`, and `diskutil`
commands. The tool does not install a privileged helper or kernel extension.

Mount session files contain local image paths, process identifiers, device
names, mount points, and server logs. Session directories are restricted to the
current user. Do not publish them in bug reports. Use `rdrkit unmount SESSION`
to recover an incomplete session before removing its state directory manually.

The NFS listener must remain bound to localhost. The managed workflow always
selects `127.0.0.1`; users of the low-level `serve` command are responsible for
not exposing disk contents to another interface.
