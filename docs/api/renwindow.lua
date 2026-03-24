---@meta

---
---Functionality to create and manage windows
---@class renwindow
renwindow = {}

---
---Create a new window.
---
---@param title? string The title given to the newly created window.
---
---@return renwindow
function renwindow.create(title) end

---
---Mark the window as persistent so it survives a restart.
---
function renwindow.persist() end

---
---Restore a previously persisted window.
---
---@return renwindow?
function renwindow._restore() end

---
---Get the logical width and height of the window.
---
---@return integer width
---@return integer height
function renwindow:get_size() end

---
---Get the ratio of drawable pixels to logical window points.
---
---@return number scale
function renwindow:get_content_scale() end

---
---Mark this window handle as persistent so it survives a restart.
---
function renwindow:_persist() end
