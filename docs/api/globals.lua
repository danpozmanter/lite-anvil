---@meta

---The command line arguments given to lite.
---@type table<integer, string>
ARGS = {}

---The current platform tuple used for native modules loading,
---for example: "x86_64-linux", "x86_64-darwin", "x86_64-windows", etc...
---@type string
ARCH = "Architecture-OperatingSystem"

---The current operating system.
---@type string | "Windows" | "Mac OS X" | "Linux" | "iOS" | "Android"
PLATFORM = "Operating System"

---The current text or ui scale.
---@type number
SCALE = 1.0

---Full path of lite executable.
---@type string
EXEFILE = "/path/to/lite"

---Path to the users home directory.
---@type string
HOME = "/path/to/user/dir"

---Whether lite-anvil was restarted rather than freshly launched.
---@type boolean
RESTARTED = false

---The version string of lite-anvil (from Cargo.toml).
---@type string
VERSION = "0.0.0"

---Native module API major version number.
---@type integer
MOD_VERSION_MAJOR = 4

---Native module API minor version number.
---@type integer
MOD_VERSION_MINOR = 0

---Native module API patch version number.
---@type integer
MOD_VERSION_PATCH = 0

---Native module API version as a string.
---@type string
MOD_VERSION_STRING = "4.0.0"

---Directory containing the lite-anvil executable.
---@type string
EXEDIR = "/path/to/exe/dir"

---Directory containing lite-anvil's bundled data files.
---@type string
DATADIR = "/path/to/data/dir"

---Directory for user configuration and plugins.
---@type string
USERDIR = "/path/to/user/dir"

---The platform-specific path separator ("/" or "\\").
---@type string
PATHSEP = "/"
