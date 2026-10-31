-- HUD widgets: shadow.hud(name, fn) adds "NAME value" to the top bar (re-evaluated every 2 s,
-- 20 ms budget). Keep them cheap: the HUD only redraws when the text changes.

shadow.hud("win", function()
  local s = shadow.state()
  if s.windows > 6 then
    return tostring(s.windows) .. " open"
  end
  return ""
end)

shadow.hud("pwr", function()
  local s = shadow.state()
  if not s.ac and s.battery >= 0 and s.battery < 15 then
    return "LOW " .. tostring(s.battery) .. "%"
  end
  return ""
end)
