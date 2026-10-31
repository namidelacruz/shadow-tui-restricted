-- Window placement rules (shadow-compositor). Return a table to override the defaults:
--   preset = 1..3 (width preset), requires_pointer = true|false, fullscreen = true|false
-- Mutable behaviour layer: shadow-healer may propose fixes to this file; core binaries never change.

local wide = { ["firefox"] = true, ["chromium"] = true, ["libreoffice"] = true, ["org.inkscape.Inkscape"] = true }

shadow.rule(function(win)
  if wide[win.app_id] then
    return { preset = 3, requires_pointer = true }
  end
  if win.app_id == "shadow-launch" then
    return { preset = 1 }
  end
  if win.app_id == "mpv" then
    return { fullscreen = true, requires_pointer = false }
  end
end)
