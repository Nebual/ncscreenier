Set oWS = WScript.CreateObject("WScript.Shell")
Set oLink = oWS.CreateShortcut(oWS.expandEnvironmentStrings("%APPDATA%") & "\Microsoft\Windows\Start Menu\Programs\Startup\ncscreenier.lnk")
    oLink.TargetPath = oWS.CurrentDirectory & "\ncscreenier.exe"
    oLink.Arguments = "--quiet --account=" & InputBox("Enter a name to store images under", "NCScreenier Setup", "anon")
    oLink.WorkingDirectory = oWS.CurrentDirectory
oLink.Save
