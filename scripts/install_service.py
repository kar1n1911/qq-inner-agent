#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
unit = Path.home() / '.config/systemd/user/qq-inner-agent.service'
unit.parent.mkdir(parents=True, exist_ok=True)
def quote(s): return '"' + str(s).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
unit.write_text(f'''[Unit]
Description=QQ Inner Thoughts conversational agent
After=network.target
StartLimitIntervalSec=0

[Service]
Type=simple
WorkingDirectory={str(root).replace('%', '%%')}
ExecStart={quote(root / 'agent')} start
Restart=always
RestartSec=5
TimeoutStopSec=25
UMask=0077
NoNewPrivileges=true
Environment=NODE_NO_WARNINGS=1
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
''')
os.chmod(unit, 0o644)
subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
subprocess.run(['systemctl', '--user', 'enable', '--now', 'qq-inner-agent.service'], check=True)
print('Installed and started qq-inner-agent.service for this user.')
print('It starts on login. To keep it alive after logout: loginctl enable-linger ' + os.environ.get('USER', '<username>'))
