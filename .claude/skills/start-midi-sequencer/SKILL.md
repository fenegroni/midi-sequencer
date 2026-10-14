---
name: start-midi-sequencer
description: Starts the MIDI Sequencer app from C:\Tools\MidiSequencer and waits until its MCP server answers. Use when the user asks to start, open, or run the sequencer, or before any midi-sequencer MCP tool call when the app is not running or the server refused the connection.
---

# Start the MIDI Sequencer

The user keeps the build at `C:\Tools\MidiSequencer\midi-sequencer.exe`. Its MCP server
listens on `http://127.0.0.1:7391/mcp` while the window is open.

## Steps

1. Check whether the app already runs. Do not start a second copy, because the second
   copy cannot open the MIDI ports or the server address.

   ```powershell
   Get-Process midi-sequencer -ErrorAction SilentlyContinue
   ```

2. If it does not run, start it.

   ```powershell
   Start-Process -FilePath "C:\Tools\MidiSequencer\midi-sequencer.exe"
   ```

   If the file is missing, stop and tell the user. Do not build or copy a replacement
   without asking.

3. Wait until the server answers a ping. Try for up to 10 seconds.

   ```powershell
   $body = '{"jsonrpc":"2.0","id":1,"method":"ping"}'
   foreach ($i in 1..20) {
     try { Invoke-WebRequest -UseBasicParsing -Method Post -Uri http://127.0.0.1:7391/mcp -ContentType 'application/json' -Body $body | Out-Null; 'ready'; break }
     catch { Start-Sleep -Milliseconds 500 }
   }
   ```

4. Claude Code connects to MCP servers when the session starts. If the app was not running
   then, the `midi-sequencer` tools are missing from the session. Reconnect the server if
   a reconnect tool is available. Otherwise, tell the user that the app runs but the session
   must reconnect to the `midi-sequencer` server before its tools appear.

5. Tell the user in one sentence that the sequencer is open.

## Notes

- The MPK mini play 3 connects by itself within two seconds when it is plugged in.
- The app keeps its notes in memory only. Closing the window loses them unless they were
  exported with `export_midi`.
