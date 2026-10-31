-- Key macros: shadow.bind(key, fn). Only layout actions, notifications and spawning are possible.

shadow.bind("Mod+Shift+N", function()
  shadow.spawn("shadow-term --app-id=shadow-net -e sakura net")
end)

shadow.bind("Mod+Shift+Space", function()
  -- jump two windows round the ring
  shadow.action("focus-next")
  shadow.action("focus-next")
end)

shadow.on("focus", function(app_id, title)
  if app_id == "shadow-net" then
    shadow.notify("NET: F fix device · c campus · R repair")
  end
end)
