-- Key macros: shadow.bind(key, fn). Only layout actions, notifications and spawning are possible.
-- shadow-sessiond turns each key into a compositor bind (core binds always win).

shadow.bind("Mod+Shift+N", function()
  shadow.spawn("shadow-net")
end)

shadow.bind("Mod+Shift+Space", function()
  -- jump two windows along the strip
  shadow.action("focus-next")
  shadow.action("focus-next")
end)

shadow.on("focus", function(app_id, title)
  if app_id == "shadow-net" then
    shadow.notify("NET: F fix device · c UC-Secure · e eduroam · R repair")
  end
end)
