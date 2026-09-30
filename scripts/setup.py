#!/usr/bin/env python3
"""Local setup. Keys are read without terminal echo and never put in argv."""
import copy
import getpass
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent

def write_private(path, data):
    tmp = path.with_suffix(path.suffix + '.tmp')
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, 'w') as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write('\n')
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)

def ask(label, default=''):
    return input(f'{label}' + (f' [{default}]' if default else '') + ': ').strip() or default

def ids(label, current):
    while True:
        value = ask(label + ' (comma-separated IDs; - clears)', ','.join(current))
        values = [] if value == '-' else [x.strip() for x in value.split(',') if x.strip()]
        if all(x.isdigit() and int(x) > 0 for x in values):
            return list(dict.fromkeys(values))
        print('Please enter numeric QQ IDs.')

def main():
    os.umask(0o077)
    cpath, spath = ROOT / 'config.json', ROOT / 'secrets.json'
    config = json.loads((cpath if cpath.exists() else ROOT / 'config.example.json').read_text())
    secrets = json.loads(spath.read_text()) if spath.exists() else {}
    print('QQ Inner Agent — local setup\nOne account, separate memory per selected group/contact.\nRun ./agent contacts to list available IDs. API keys stay on this computer.')
    profile = ask('Provider: deepseek-openai / deepseek-anthropic / custom', 'deepseek-openai')
    p = config.setdefault('provider', {})
    if profile.startswith('deepseek-'):
        if profile not in ('deepseek-openai', 'deepseek-anthropic'):
            raise ValueError('Unknown provider profile')
        p.update(kind='anthropic' if profile.endswith('anthropic') else 'openai',
                 baseUrl='https://api.deepseek.com/anthropic' if profile.endswith('anthropic') else 'https://api.deepseek.com',
                 tokenParameter='max_tokens', thinking='disabled')
        p['model'] = ask('DeepSeek model', p.get('model') or 'deepseek-flash')
    elif profile == 'custom':
        p['kind'] = ask('API format: openai / anthropic', p.get('kind', 'openai'))
        p['baseUrl'] = ask('API base URL', p.get('baseUrl', ''))
        p['model'] = ask('Model ID', p.get('model', ''))
        p['thinking'] = None
        p['tokenParameter'] = ask('OpenAI token parameter: max_tokens / max_completion_tokens', 'max_completion_tokens')
    else:
        raise ValueError('Unknown provider profile')
    value = getpass.getpass('API key (hidden; Enter keeps existing): ').strip()
    if value: secrets['apiKey'] = value
    a = config.setdefault('agent', {})
    a['allowedGroups'] = ids('Enabled group IDs', a.get('allowedGroups', []))
    a['allowedUsers'] = ids('Enabled private contact IDs', a.get('allowedUsers', []))
    a['proactive'] = ask('Proactive participation? yes / no', 'yes' if a.get('proactive', True) else 'no').lower() == 'yes'
    a['name'] = ask('Agent display name', a.get('name', 'Luma'))
    a['aliases'] = [a['name']]
    a['persona'] = ask('Persona / conversational purpose', a.get('persona', 'A helpful, concise AI participant.'))
    a['dryRun'] = ask('Preview decisions without sending messages? yes / no', 'no').lower() == 'yes'
    # Validate using the exact runtime validator before replacing the working config.
    node = str(ROOT / '.runtime/node') if (ROOT / '.runtime/node').exists() else 'node'
    check = subprocess.run([node, '--input-type=module', '-e',
        "import {defaults,merge,validate} from './src/config.mjs';import fs from 'node:fs';try{validate(merge(defaults,JSON.parse(fs.readFileSync(0,'utf8'))))}catch(e){console.error(e.message);process.exit(1)}"],
        input=json.dumps(config), text=True, cwd=ROOT)
    if check.returncode: raise ValueError('Configuration not saved; fix the values above.')
    write_private(cpath, config)
    write_private(spath, secrets)
    print('Saved config.json and secrets.json (owner-only).')
    if not secrets.get('apiKey'): print('Waiting for an API key. You can rerun setup.')
    if not a['allowedGroups'] and not a['allowedUsers']: print('No chats selected: no conversation will be sent to the model or QQ.')
    service = Path.home() / '.config/systemd/user/qq-inner-agent.service'
    if service.exists():
        subprocess.run(['systemctl', '--user', 'restart', 'qq-inner-agent.service'], check=True)
        print('Background service restarted. Run ./agent status or ./agent logs.')
    else: print('Next: ./agent check --api, then ./agent install-service')

if __name__ == '__main__':
    try: main()
    except (KeyboardInterrupt, EOFError): print('\nSetup cancelled.')
    except Exception as e: print(f'Setup failed: {e}'); raise SystemExit(1)
