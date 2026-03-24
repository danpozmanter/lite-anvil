---@meta

---
---Functionality that allows you to launch subprocesses and read
---or write to them in a non-blocking fashion.
---@class process
process = {}

---Used for the process:close_stream() method to close stdin.
---@type integer
process.STREAM_STDIN = 0

---Used for the process:close_stream() method to close stdout.
---@type integer
process.STREAM_STDOUT = 1

---Used for the process:close_stream() method to close stderr.
---@type integer
process.STREAM_STDERR = 2

---Do not wait; return immediately if the process has not exited.
---@type integer
process.WAIT_NONE = 0

---Instruct process:wait() to wait until the deadline given on process:start().
---@type integer
process.WAIT_DEADLINE = -1

---Instruct process:wait() to wait until the process ends.
---@type integer
process.WAIT_INFINITE = -2

---Default behavior for redirecting streams.
---@type integer
process.REDIRECT_DEFAULT = -1

---Redirect this stream to stdout.
---This flag can only be used on process.options.stderr.
---@type integer
process.REDIRECT_STDOUT = 1

---Redirect this stream to stderr.
---This flag can only be used on process.options.stdout.
---@type integer
process.REDIRECT_STDERR = 2

---Redirect this stream to the parent.
---@type integer
process.REDIRECT_PARENT = -3

---Discard this stream (piping it to /dev/null).
---@type integer
process.REDIRECT_DISCARD = -2

---@alias process.streamtype
---| `process.STREAM_STDIN`
---| `process.STREAM_STDOUT`
---| `process.STREAM_STDERR`

---@alias process.waittype
---| `process.WAIT_NONE`
---| `process.WAIT_INFINITE`
---| `process.WAIT_DEADLINE`

---@alias process.redirecttype
---| `process.REDIRECT_DEFAULT`
---| `process.REDIRECT_STDOUT`
---| `process.REDIRECT_STDERR`
---| `process.REDIRECT_PARENT`
---| `process.REDIRECT_DISCARD`

---
--- Options that can be passed to process.start()
---@class process.options
---@field public timeout number
---@field public cwd string
---@field public stdin process.redirecttype
---@field public stdout process.redirecttype
---@field public stderr process.redirecttype
---@field public env table<string, string>

---
---Create and start a new process
---
---@param command_and_params table First index is the command to execute
---and subsequente elements are parameters for the command.
---@param options process.options
---
---@return process | nil
---@return string errmsg
---@return integer errcode
function process.start(command_and_params, options) end

---
---Translates an error code into a useful text message
---
---@param code integer
---
---@return string | nil
function process.strerror(code) end

---
---Get the process id.
---
---@return integer id Process id or 0 if not running.
function process:pid() end

---
---Read from the given stream type, if the process fails with a ERROR_PIPE it is
---automatically destroyed returning nil along error message and code.
---
---@param stream process.streamtype
---@param len? integer Amount of bytes to read, defaults to 2048.
---
---@return string | nil
---@return string errmsg
---@return integer errcode
function process:read(stream, len) end

---
---Read from stdout, if the process fails with a ERROR_PIPE it is
---automatically destroyed returning nil along error message and code.
---
---@param len? integer Amount of bytes to read, defaults to 2048.
---
---@return string | nil
---@return string errmsg
---@return integer errcode
function process:read_stdout(len) end

---
---Read from stderr, if the process fails with a ERROR_PIPE it is
---automatically destroyed returning nil along error message and code.
---
---@param len? integer Amount of bytes to read, defaults to 2048.
---
---@return string | nil
---@return string errmsg
---@return integer errcode
function process:read_stderr(len) end

---
---Write to the stdin, if the process fails with a ERROR_PIPE it is
---automatically destroyed returning nil along error message and code.
---
---@param data string
---
---@return integer | nil bytes The amount of bytes written or nil if error
---@return string errmsg
---@return integer errcode
function process:write(data) end

---
---Allows you to close a stream pipe that you will not be using.
---
---@param stream process.streamtype
---
---@return integer | nil
---@return string errmsg
---@return integer errcode
function process:close_stream(stream) end

---
---Wait the specified amount of time for the process to exit.
---
---@param timeout integer | process.waittype Time to wait in milliseconds,
---if 0, the function will only check if process is running without waiting.
---
---@return integer | nil exit_status The process exit status or nil on error
---@return string errmsg
---@return integer errcode
function process:wait(timeout) end

---
---Sends SIGTERM to the process
---
---@return boolean | nil
---@return string errmsg
---@return integer errcode
function process:terminate() end

---
---Sends SIGKILL to the process
---
---@return boolean | nil
---@return string errmsg
---@return integer errcode
function process:kill() end

---
---Sends SIGINT to the process.
---
---@return boolean
function process:interrupt() end

---
---Get the exit code of the process or nil if still running.
---
---@return number | nil
function process:returncode() end

---
---Check if the process is running
---
---@return boolean
function process:running() end


return process
