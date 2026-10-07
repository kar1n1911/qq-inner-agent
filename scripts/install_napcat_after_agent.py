#!/usr/bin/env python3
"""让 NapCat 在 agent 之后启动(用 systemd drop-in,不改 NapCat 原 unit)。

为什么是这个方向:NapCat 启动/重连时会由 QQ 客户端同步一批历史消息并实时推给
当前连着的 OneBot 客户端。如果 NapCat 先起、agent 后连,这批同步消息就推给了
空连接、永久丢失(实测 19:41 那批 14 条)。让 agent 先起并在重连中,NapCat 再起,
消息就能推给已经连上的 agent。

只加 `After=`(顺序)与 `Wants=`(一起拉起),不强制依赖:NapCat 仍是弱依赖 agent。
只做 daemon-reload,不重启 NapCat(避免重新登录 QQ)。
"""
import subprocess
from pathlib import Path

drop = Path.home() / '.config/systemd/user/napcat.service.d'
drop.mkdir(parents=True, exist_ok=True)
(drop / 'after-agent.conf').write_text('''[Unit]
# agent 先起并在重连中,NapCat 再起:QQ 客户端启动/重连时同步的历史消息
# 才能推给已连上的 agent,而不是推给空连接后丢失。
After=qq-inner-agent.service
Wants=qq-inner-agent.service
''')
subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
print('NapCat drop-in installed: it now starts after qq-inner-agent.service.')
print('Takes effect on the next start of napcat.service (no restart performed).')
