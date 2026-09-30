#!/usr/bin/env python3
"""Install an authenticated HTTPS dashboard on the host's LAN/VPN addresses."""
import ipaddress
import json
import os
from pathlib import Path
import subprocess

os.umask(0o077)
root = Path(__file__).resolve().parent.parent
data = root / 'data'
data.mkdir(mode=0o700, exist_ok=True)
ips = []
for raw in subprocess.check_output(['hostname', '-I'], text=True).split():
    ip = ipaddress.ip_address(raw)
    if ip.version == 4: ips.append(str(ip))
names = list(dict.fromkeys(['127.0.0.1'] + ips))
ca_key, ca_cert = data / 'dashboard-ca.key', root / 'dashboard-ca.crt'
server_key, server_cert = data / 'dashboard-server.key', data / 'dashboard-server.crt'
def run(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
if not ca_key.exists():
    run('openssl', 'req', '-x509', '-newkey', 'rsa:3072', '-nodes', '-keyout', str(ca_key), '-out', str(ca_cert), '-days', '3650', '-subj', '/CN=Luma Dashboard Local CA', '-addext', 'basicConstraints=critical,CA:TRUE', '-addext', 'keyUsage=critical,keyCertSign,cRLSign')
csr = data / 'dashboard-server.csr'
run('openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(server_key), '-out', str(csr), '-subj', '/CN=Luma Agent Dashboard')
ext = data / 'dashboard-server.ext'
ext.write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,' + ','.join('IP:' + ip for ip in names) + '\n')
run('openssl', 'x509', '-req', '-in', str(csr), '-CA', str(ca_cert), '-CAkey', str(ca_key), '-CAcreateserial', '-out', str(server_cert), '-days', '365', '-sha256', '-extfile', str(ext))
os.chmod(ca_cert, 0o644)
settings = {'host': '0.0.0.0', 'port': 5098, 'localPort': 5097,
            'origins': [f'https://{ip}:5098' for ip in names] + ['https://localhost:5098', 'http://localhost:5097', 'http://127.0.0.1:5097'],
            'tls': {'key': 'data/dashboard-server.key', 'cert': 'data/dashboard-server.crt'}}
(root / 'dashboard.json').write_text(json.dumps(settings, indent=2) + '\n')
unit = Path.home() / '.config/systemd/user/qq-inner-dashboard.service'
unit.parent.mkdir(parents=True, exist_ok=True)
exe = str(root / 'agent').replace('%', '%%').replace('"', '\\"')
unit.write_text(f'''[Unit]
Description=Luma QQ agent HTTPS dashboard
After=network.target
StartLimitIntervalSec=0

[Service]
Type=simple
WorkingDirectory={str(root).replace('%', '%%')}
ExecStart="{exe}" dashboard
Restart=always
RestartSec=5
TimeoutStopSec=10
UMask=0077
NoNewPrivileges=true
Environment=NODE_NO_WARNINGS=1

[Install]
WantedBy=default.target
''')
os.chmod(unit, 0o644)
subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
subprocess.run(['systemctl', '--user', 'enable', '--now', unit.name], check=True)
subprocess.run(['systemctl', '--user', 'restart', unit.name], check=True)
print('Dashboard installed. Local browser: http://localhost:5097')
for ip in ips: print(f'LAN/VPN HTTPS: https://{ip}:5098')
print('Trust dashboard-ca.crt on remote devices. Retrieve the access key with ./agent dashboard-key.')
