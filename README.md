<div align="center">

<h1>OUTNO</h1>

<img src="assets/outno-icon.png" alt="OUTNO application icon" width="150">

<h3>Windows Installer Builder</h3>

<p><strong>Build. Package. Install.</strong></p>

<p>Create self-contained Windows setup executables from your application files, with a familiar installation wizard and an accompanying uninstaller.</p>

<p>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Built%20with-Rust-orange?logo=rust" alt="Built with Rust"></a>
  <a href="https://www.microsoft.com/windows"><img src="https://img.shields.io/badge/Platform-Windows-0078D4?logo=windows" alt="Windows"></a>
  <a href="#project-status"><img src="https://img.shields.io/badge/Status-Early%20development-orange" alt="Early development"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="MIT License"></a>
</p>

<p>
  <strong>
    <a href="https://github.com/floringhitun223/outno/releases">Download latest release</a>
    · <a href="https://github.com/floringhitun223/outno/releases">All releases</a>
    · <a href="https://github.com/floringhitun223/outno/issues">Report a bug</a>
    · <a href="https://github.com/floringhitun223/outno/issues">Request a feature</a>
  </strong>
</p>

</div>

------------------------------------------------------------------------

## About OUTNO

OUTNO is a Windows installer creation tool written in Rust. It takes an
application directory and packages its files into a standalone Windows
setup executable.

The generated setup contains the installer runtime, application archive
and manifest in a single executable. When launched, it presents an
installation wizard, extracts the application and creates the selected
shortcuts. An uninstaller is installed alongside the application.

OUTNO aims to provide a straightforward, self-contained workflow for
building Windows application installers.

## Project status

OUTNO is in early development. The features described below reflect the
current project implementation and documented behavior. Items in the
roadmap are planned work, not existing features.

## Features

- **Self-contained setup executable** — combines the installer runtime,
  application archive and manifest into one file.
- **Graphical installer wizard** — provides Welcome, License,
  Destination, Tasks, Ready, Installing and Finish stages.
- **Per-user installation** — installs under
  `%LOCALAPPDATA%\Programs\<App>` without requiring administrator
  privileges.
- **Application shortcuts** — creates Desktop and Start Menu shortcuts.
- **Uninstaller generation** — places an uninstaller executable and its
  data file in the installation directory.
- **Windows app registration** — creates an Add/Remove Programs entry
  for the current user.
- **Archive path protection** — blocks archive paths that attempt to
  escape the installation directory.
- **Native Windows target** — built with Rust for Windows.

## How it works

OUTNO has three operating modes:

| Mode            | Purpose                                                                  |
|-----------------|--------------------------------------------------------------------------|
| **Compiler**    | Select an application folder and configure the installer.                |
| **Installer**   | Run the generated setup executable and install the packaged application. |
| **Uninstaller** | Confirm and remove an installed application.                             |

### Installer package format

The generated setup executable uses an appended-payload format:

``` text
[installer executable]
[application ZIP archive]
[manifest.json]
[ZIP length]
[manifest length]
["RSINST01"]
```

At startup, the executable checks for the payload trailer. When a valid
installer payload is detected, it launches the installation wizard and
extracts the application archive.

## Getting started

### Requirements

- Windows
- [Rust](https://www.rust-lang.org/tools/install) and Cargo
- Microsoft C++ Build Tools for the `x86_64-pc-windows-msvc` target

### Build from source

Install the Windows MSVC target if it is not already available:

``` powershell
rustup target add x86_64-pc-windows-msvc
```

Build OUTNO in release mode:

``` powershell
cargo build --release
```

To build and launch the application in one step:

``` powershell
cargo run --release
```

The compiled executable is placed in the Cargo release output directory:

``` text
target\release\
```

The executable filename is determined by the binary target configured in
`Cargo.toml`.

## Creating an installer

1.  Launch OUTNO.
2.  Select the **Application folder** containing the files you want to
    distribute.
3.  Review the detected application name and main executable.
4.  Optionally select a `.txt` license file.
5.  Configure the shortcut options and output folder.
6.  Select **Build installer**.
7.  Find the generated setup executable in the selected output
    directory.

The generated installer filename follows this pattern:

``` text
<App>-setup-<version>.exe
```

## Installation behavior

The currently documented installer behavior is:

- Installs per-user by default to `%LOCALAPPDATA%\Programs\<App>`.
- Creates Desktop and Start Menu `.lnk` shortcuts.
- Writes `unins000.exe` and `unins000.dat` into the installation
  directory.
- Creates an Add/Remove Programs entry under `HKCU`.
- Blocks `../` path traversal during archive extraction.

An all-users installation mode and further installer customization are
planned, but are not documented as implemented.

## Roadmap

Potential next steps for OUTNO include:

- [ ] Embed an application icon and Windows UAC manifest.
- [ ] Add an all-users installation option targeting `Program Files`.
- [ ] Add installer language packs.
- [ ] Support custom wizard images.
- [ ] Support rich-text (`.rtf`) license files.
- [ ] Save and load installer projects.
- [ ] Introduce a script-based project format.
- [ ] Support code-signing workflows for generated installers.

The roadmap may change as development progresses.

## Contributing

Contributions, bug reports and feature suggestions are welcome.

Before opening a pull request:

1.  Check existing issues to avoid duplicate reports.
2.  Describe the problem or proposed change clearly.
3.  Include reproduction steps for bug fixes where possible.
4.  Keep changes focused and document user-facing behavior.

Use the [Issues](https://github.com/floringhitun223/outno/issues) page
to report bugs or suggest features.

## Security

Installer software handles executable files, archives and filesystem
operations. If you discover a potential security vulnerability, please
contact the project maintainer privately rather than publishing exploit
details in a public issue.

Only package software that you own or have permission to distribute.
Test generated installers in a safe environment before distributing
them.

## License

OUTNO is licensed under the [MIT License](LICENSE).

------------------------------------------------------------------------

<div align="center">

<strong>OUTNO</strong> · Build. Package. Install.

</div>
