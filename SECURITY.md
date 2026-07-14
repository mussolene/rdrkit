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
