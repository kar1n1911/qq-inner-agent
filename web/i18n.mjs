// Only application-owned UI text is translated. Chat contents, model IDs, logs,
// configuration editors and other user-provided values remain untouched.
const pairs = `
Advanced|高级
Owner private teaching commands /记住 /黑话 /还原|主人私聊教学指令 /记住 /黑话 /还原
Owner QQ ID|主人 QQ 号
Feature switches|功能开关
Emotions (mood / affinity)|情绪（心情/好感）
Deeper memory recall|记忆召回下钻
Three-layer decisions|三层决策
Learn emoji frequency|表情频率学习
Emoji-only replies|只发表情
Multiple message bubbles|多气泡
Cross-group forwarding|群间转发
Send and receive merged forwards|合并转发收发
Autonomous identity|身份自治
Persona growth|人格成长
Allow nickname changes|允许修改昵称
Allow group card changes|允许修改群名片
Allow avatar changes|允许修改头像
Allow signature changes|允许修改签名
Past scenarios|过往情景
Advanced parameters|高级参数
Identity|身份自治
Minimum identity traits|身份自治最少特征数
Minimum identity age (days)|身份自治最短积累天数
Identity cooldown (days)|身份修改冷却天数
Relay|群间转发
Relay relevance threshold|转发相关度阈值
High-risk relay threshold|高风险转发阈值
Maximum merged messages|合并消息数上限
Allow high-risk forwarding|允许高风险转发
External topic sources|外部话题来源
Topic relevance threshold|话题相关度阈值
Topic fetch interval (hours)|话题抓取间隔（小时）
Topics per hour|每小时话题上限
Topic request budget|话题请求次数上限
Topic item budget|话题条目上限
Total topic character budget|话题总字符上限
Topic cache lifetime (hours)|话题缓存时长（小时）
GitHub queries (one per line)|GitHub 查询（每行一项）
RSS feed URLs (one per line)|RSS 订阅地址（每行一项）
Good morning|早上好
Good afternoon|下午好
Good evening|晚上好
Participation principles|参与行为准则
Base reply style|基础回复风格
Interests (one per line)|兴趣方向（每行一项）
Alternative styles (one per line)|备用表达风格（每行一项）
Alternative style probability|临时采用备用风格的概率
Expressions, jargon & emoji|表达、黑话与表情
Learn expressions and jargon|学习当前聊天的表达和黑话
Use learned expressions and jargon|使用已学习的表达和黑话
Allow optional emoji decorations|允许可选表情装饰
Minimum expression confidence|表达最低可信度
Expression candidates per reply|每轮参考表达数
Expression entries per chat|每个聊天的表达条目上限
Expression retention (days)|表达保留时间（天）
Expression reuse interval (seconds)|相同表达复用间隔（秒）
Emoji opportunity probability|本轮允许表情的概率
Emoji interval (seconds)|表情使用间隔（秒）
Text emoji (one per line)|文字表情（每行一个）
Allowed QQ face IDs|允许的 QQ 原生表情编号
Learned expressions & jargon|已学习的表达与黑话
No learned expressions yet.|暂无已学习表达。
Applicable situation|适用场景
Observed example|原话示例
Confidence|可信度
Evidence messages|证据消息数
Reset this subject’s learning|重置此主体的记忆与表达
jargon|黑话
expression|表达习惯
Learn only from attributed messages in this chat. At least two messages are needed before use; group patterns also require two speakers. Learning follows the memory cadence and master switch. Images and stickers are not collected.|只从当前聊天中来源明确的消息学习。至少两条证据才可使用；群体表达还需两位发言人。学习遵守记忆总开关与学习间隔。不自动收集图片或表情包。

Memory recall character budget|记忆召回字数上限
Recall recency half-life (days)|召回时效半衰期（天）
Minimum memory confidence|记忆最低可信度
Retained memory revisions|保留的记忆修订版本数

Without activity rhythm, all AI replies pause outside this window, including mentions and private messages. With rhythm enabled, this window shapes the next block probability. Overnight windows are supported. Quiet hours below separately suppress proactive replies.|未启用连续节奏时，作息外暂停所有回复（含私聊和 @）。启用连续节奏后，此时间表用于决定下一段的活跃概率。支持跨午夜时段。下方静默时段仍单独限制主动发言。
Continuous activity rhythm|连续活跃与休息节奏
Enable probabilistic activity blocks|启用概率性活跃时段
Daytime activity probability|活跃时间内的活跃概率
Inactive edge activity probability|非活跃时间两端的活跃概率
Inactive center activity probability|非活跃时间中央的活跃概率
Inactivity curve width|休息概率曲线宽度
Minimum active block (seconds)|最短活跃时段（秒）
Maximum active block (seconds)|最长活跃时段（秒）
Minimum rest block (seconds)|最短休息时段（秒）
Maximum rest block (seconds)|最长休息时段（秒）
Active block|连续活跃中
Rest block|连续休息中
Next block selection|下次状态抽取时间
Activity probability at selection|抽取时活跃概率
Current curve probability|当前曲线活跃概率
AI participation is paused by the activity schedule or rest block.|当前受作息或连续休息时段限制，暂不参与聊天。
Overrides the strict schedule gate. Rest is most likely in the middle of inactive hours; activity is more likely near either edge. One state is shared by all chats until the block ends, including mentions and private replies. Quiet hours still restrict proactive messages. With the daily schedule disabled, the daytime probability applies all day.|替代严格的作息禁言。非活跃时间中央最容易休息，两端更容易活跃。所有聊天共用同一状态并保持到本段结束，私聊和 @ 也受限制。静默时段仍限制主动发言。关闭每日作息时，全天使用活跃时间内的概率。
The probability selects the next block, not each message. Adjacent blocks may have the same state. Existing blocks survive restarts and may cross daily schedule boundaries; changing rhythm or schedule settings starts a new block.|概率用于抽取下一段状态，不对每条消息重复抽签。相邻时段可能保持同一状态。当前时段在重启后保留，也可跨越作息边界；修改节奏或作息配置会重新开始一段。

Group observation period|入群观察期
Observe before the first group message|群内首次发言前先观察
Minimum observation time (seconds)|最少观察时间（秒）
Minimum new messages|最少新消息数
Threshold rule|阈值规则
Both time and message count|时间和消息量均达到
Either time or message count|时间或消息量任一达到
History sample size|历史消息采样条数
Direct mentions also wait. Available group information, announcements and history are analyzed before the model chooses its initial style. Imported history does not count as new messages.|被 @ 也需等待。先读取可用群资料、公告和历史，再由模型分析并选择初始说话风格。读取的历史不计入新消息数。
No group observation yet.|暂无群观察记录。
Group name unavailable|群名称尚不可用
Style selected|已选择初始风格
Observing before first message|观察中，首次发言暂缓
Elapsed seconds|已观察秒数
Observation duration|观察用时
New messages|新消息数
Group information|群资料
Group announcements|群公告
Group history|群历史
Initial speaking style|初始说话风格
available|已获取
unavailable|不可用
pending|待获取
Analysis failed; waiting to retry. No group message will be sent.|分析未成功，等待重试；暂不发送群消息。
Group memory|群体记忆
Long-term notebook|长期笔记本
Short-term details|短期细节
Traits and topics|特征与主题
Short-term lifetime (hours)|短期记忆保留小时数
Short-term entries per scope|每个主体的短期记忆条数
Characters per short-term entry|每条短期记忆字数
Notebook capacity (characters)|每个主体的长期笔记本字数
Trait capacity (characters)|每个主体的特征档案字数
Long-term lifetime (days)|长期笔记本保留天数
Trait lifetime (days)|特征档案保留天数
People retained per chat|每个聊天保留的个人档案数
Adaptive persona & memory|自适应角色与记忆
Learn chat style and useful memories|学习聊天风格与有用记忆
Group and personal memory are isolated by chat and QQ ID. Each has a long-term notebook, short-term details and traits. Only the current group and speaker notebooks are loaded.|分别维护群体与个人的长期笔记本、短期细节和特征。群内个人档案按群号与 QQ 号隔离，私聊档案独立；只加载当前群体和当前发言人的笔记本。基础角色保持优先。
New messages before learning|学习前至少收到的新消息数
Learning interval (seconds)|学习最短间隔（秒）
Memories per chat|每个聊天的记忆上限
Memory lifetime (days)|记忆保留天数
Retrieved memories per turn|每轮检索条数
Learned chat styles & memories|已学习的聊天风格与记忆
Shows up to 200 entries, with notebooks and traits first, separated by chat and subject. Reset clears only that subject. Original chat history remains; disable learning to stop memory use and updates.|展示最多200条当前记忆，优先展示长期与特征层，按聊天和主体分开。重置清除此主体的三层记忆和表达黑话，原始聊天记录保留；关闭学习可停止写入和使用记忆。
No learned chat preferences yet.|暂无已学习的聊天偏好。
Reset learned style and memories|重置此主体的记忆与表达
Learned style and memories reset.|已重置学习风格与记忆。
No learned style yet.|暂无已学习的风格。
Style sources|风格依据
Sending policy & predictions|发送策略与预测
Enable probability and prediction checks|启用发送概率与预测检查
Probabilities are heuristic estimates. Timing and chat pace reduce proactive participation. A withheld attempt is not retried during silence. Direct requests bypass timing factors, but still respect the forecast veto and configured probability.|概率是启发式估计。等待时间、发言间隔与消息密度会降低主动发言概率。放弃发送后，不会因沉默反复尝试。直接提问不受时间因子影响，但仍遵守预测否决与所设概率。
Proactive base probability|主动发言基础概率
Addressed base probability|被点名时的基础概率
Conversation settling time (seconds)|聊天等待时间（秒）
Participation recovery time (seconds)|参与概率恢复时间（秒）
Messages per minute for half pace factor|节奏因子降至一半时的每分钟消息数
Maximum negative reaction probability|允许的最高负面反应概率
Expectation lifetime (seconds)|预期保留时间（秒）
Sending forecasts|发送预测记录
No sending forecasts yet.|暂无发送预测记录。
Send probability|发送概率
Expected response|预期回应
Reply|正常回应
Silence|没有回应
Negative reaction|负面反应
Calculation details|计算详情
admitted|允许发送
withheld|放弃发送
cancelled|已取消
sent|已发送
dry_run|仅预览
failed|发送失败
uncertain|发送结果不确定
generation_failed|生成失败
Language settings|语言设置
Interface language|界面语言
Reply language|回复语言
Follow conversation|跟随聊天语言
System prompts for formation, evaluation and articulation use Chinese. Reply language is independent of interface language. Save to apply reply settings to the agent.|候选生成、评估和最终表达均使用中文系统提示词。回复语言与界面语言独立，保存后对机器人生效。
Your agent,|你的聊天助手，
within reach.|随时掌握。
Manage conversations, tune participation, and follow what’s happening.|管理聊天、调整参与方式，查看运行状态。
Dashboard access key|控制台访问密钥
Open console|进入控制台
On the host computer, run|在运行服务的电脑上执行
to retrieve your key.|获取访问密钥。
Listen.|倾听。
Consider.|构思。
Contribute.|交流。
A little more thoughtful, by design.|先想清楚，再开口。
AGENT CONSOLE|机器人控制台
Console sections|控制台导航
Overview|概览
Configuration|配置
Activity & logs|活动与日志
Live connection|实时连接
Sign out ↗|退出登录 ↗
YOUR QQ COMPANION|你的 QQ 聊天伙伴
Connecting|正在连接
Connecting…|正在连接…
Refresh ↻|刷新 ↻
Debug: send to myself|调试：发送给自己
Sends one marked text message to the logged-in QQ account itself using the saved bridge settings. No AI call. The result confirms bridge acceptance; check QQ for delivery.|使用已保存的连接配置，向当前登录的 QQ 账号自己发送一条标记消息，不调用 AI。成功表示桥接服务接受请求，请在 QQ 中确认送达。
Send self-test message|发送自测消息
Debug: receive my messages|调试：接收自己的消息
Listen for 60 seconds, then send several messages from the same QQ account: text, image, voice, file, or a reply. Use your self-chat or another chat. Enable self-message reporting in NapCat / SnowLuma. Shows only your own events, text previews and attachment types; attachments are not downloaded. No AI call or stored history.|开始监听后，在 60 秒内用同一 QQ 账号向自己或其他聊天发送文字、图片、语音、文件或引用回复。请在 NapCat / SnowLuma 开启自身消息上报。仅展示自己的事件、文字预览和附件类型，不下载附件、不调用 AI、不保存聊天记录。
Start receiving test|开始接收测试
Stop receiving|停止接收
Not started.|尚未开始。
No events captured.|尚未收到事件。
Agent service|机器人服务
Checking service|正在检查服务
QQ connection|QQ 连接
Waiting for status|等待状态
Model provider|模型服务
Enabled chats|已启用聊天
Groups + private contacts|群聊与私聊
Room to think.|留出思考空间。
A reason to speak.|找到开口的理由。
INNER THOUGHTS|内在想法
The agent listens, develops possible contributions, and chooses when they’re worth sharing.|机器人倾听对话，构思候选发言，并判断何时值得分享。
Listen|倾听
Consider|构思
Evaluate|评估
Contribute|发言
Motivation threshold|发言动机阈值
Proactive cooldown|主动发言冷却
API calls this run|本次运行 API 调用数
Tune participation →|调整参与方式 →
Service controls|服务控制
Checking setup…|正在检查配置…
Start|启动
Restart|重启
Stop|停止
Check your model|测试模型
Uses the saved settings for one small API request. No QQ message is sent.|使用已保存配置发起一次小型 API 请求，不发送 QQ 消息。
Test API connection ↗|测试 API 连接 ↗
Recent participation decisions|最近参与决策
View activity →|查看活动 →
No decisions yet. Activity will appear when the agent processes an enabled chat.|暂无决策。机器人处理已启用聊天后将在此显示活动。
Make the agent your own|配置你的机器人
Save to apply validated changes live. The dashboard stays connected.|保存并校验后实时应用，控制台保持连接。
Discard edits|放弃修改
Save & apply|保存并应用
Active and inactive times|活跃与休息时间
Follow a daily activity schedule|启用每日活动时间表
Active from|开始活跃时间
Inactive from|开始休息时间
Schedule time zone|活动时间表时区
Outside this window, all AI replies pause, including mentions and private messages. Incoming messages during inactivity are skipped. Overnight windows are supported. Disable for all-day availability. Quiet hours below separately suppress proactive replies.|活跃时段之外暂停所有 AI 回复，包括私聊和 @ 提问，并跳过此时收到的消息。支持跨午夜时段；关闭后全天可用。下方的免打扰时段单独限制主动发言。
Choose a model|选择模型
Save the provider URL, format and API key first, then load its model list. Choose a model here or type its ID in Model & API below, then Save & apply. If a gateway has no model-list endpoint or omits your model, use manual entry.|先保存服务地址、接口格式和 API 密钥，再加载模型列表。在此选择模型，或在下方“模型与 API”中手动填写模型 ID，然后保存并应用。服务不支持列表或未列出模型时可手动填写。
Load available models|加载可用模型
Available models|可用模型
Choose a model…|请选择模型…
QQ CONNECTION|QQ 连接
Connect to a OneBot v11 WebSocket server. In NapCat, enable a WebSocket server in Network configuration with message format array. Save before loading contacts.|连接 OneBot v11 正向 WebSocket 服务。在 NapCat 的网络配置中启用 WebSocket 服务端，并选择 array 消息格式。请先保存，再加载联系人。
OneBot WebSocket URL|OneBot WebSocket 地址
QQ account ID|QQ 账号
Blank detects the connected account|留空则自动识别已连接账号
OneBot access token|OneBot 访问令牌
optional replacement; never displayed|可选替换，已保存令牌不回显
Leave blank to keep current token|留空保留当前令牌
01 / CONNECTION|01 / 模型连接
Model & API|模型与 API
API format|API 格式
OpenAI-compatible|OpenAI 兼容
Anthropic-compatible|Anthropic 兼容
Model|模型 ID
API base URL|API 基础地址
API key|API 密钥
Leave blank to keep the saved key|留空保留已保存的密钥
Output token budget|输出 token 上限
API calls / hour|每小时 API 调用上限
Use DeepSeek defaults for this format ↗|使用此格式的 DeepSeek 默认配置 ↗
02 / IDENTITY|02 / 身份设定
Voice & purpose|角色与用途
Agent name|机器人名称
Persona & conversational purpose|角色设定与聊天目的
Use a more assertive conversational tone|使用更直接的聊天语气
03 / CONVERSATIONS|03 / 聊天范围
Where the agent participates|机器人参与哪些聊天
Load QQ contacts ↻|加载 QQ 联系人 ↻
Only enabled chats are processed. Memory is separate for every group and private contact.|只处理已启用的聊天，每个群和私聊的记忆彼此独立。
Enabled group IDs|启用的群号
Comma-separated group IDs|多个群号以逗号分隔
Enabled private contact IDs|启用的私聊 QQ 号
Comma-separated QQ IDs|多个 QQ 号以逗号分隔
04 / PARTICIPATION|04 / 参与方式
When to join in|何时参与聊天
Allow proactive contributions|允许主动发言
Preview mode — record decisions without sending|预览模式：记录决策但不发送消息
More talkative|更愿意发言
More selective|更谨慎发言
Interruption threshold|插话阈值
Proactive cooldown (seconds)|主动发言冷却（秒）
Proactive messages / hour|每小时主动发言上限
All messages / hour|每小时全部消息上限
Pause trigger (seconds)|沉默触发时间（秒）
Reply length (characters)|回复长度上限（字符）
05 / AVAILABILITY|05 / 免打扰
Quiet hours|免打扰时段
Direct questions can still receive replies. Unsolicited contributions pause during this window.|此时段暂停主动发言；直接提问仍可回复，但须处于活跃时段。
Enable quiet hours|启用免打扰时段
Start hour (0–23)|开始小时（0–23）
End hour (0–23)|结束小时（0–23）
Time zone|时区
Changes take effect live|修改实时生效
The agent cancels obsolete generation work before applying changes. Editing the QQ connection reconnects its transport. Existing memory is preserved.|应用配置前会取消过期的生成任务。修改 QQ 连接配置会重新连接，已有记忆保留。
No pending changes|暂无待应用修改
Advanced configuration|高级配置
Full configuration JSON. Use “Load form into JSON” before editing here. When enabled, this editor replaces the form values.|完整 JSON 配置。编辑前先点击“将表单载入 JSON”。勾选后，保存时将以 JSON 内容为准。
Save from the JSON editor|使用 JSON 编辑器内容保存
Load form into JSON|将表单载入 JSON
Configuration JSON|JSON 配置
Remove the saved model API key on save|保存时删除模型 API 密钥
Retained candidate ideas|保留的候选发言
NOT SENT|尚未发送
Short application-generated contributions awaiting reevaluation.|等待重新评估的简短候选发言。
No retained ideas yet.|暂无保留候选。
No retained ideas yet. Ideas that are withheld can remain here for later reevaluation.|暂无保留候选。暂不发送的想法会保留在此，以便后续重新评估。
Live operational logs|实时运行日志
Pause display|暂停显示
Connection events, configuration changes, decisions, and errors. Credentials are redacted.|显示连接事件、配置变更、决策与错误。凭据已脱敏。
Filter logs|筛选日志
Filter events…|筛选事件…
Waiting for events…|等待事件…
Unsaved changes|尚未保存的修改
Saved securely|已安全保存
Not configured|尚未配置
Running|运行中
Stopped|已停止
No recent agent heartbeat|暂无近期心跳
Online|在线
Disconnected|未连接
Waiting for NapCat / SnowLuma|等待 NapCat / SnowLuma 连接
Not set|尚未设置
OpenAI-compatible API|OpenAI 兼容接口
Anthropic-compatible API|Anthropic 兼容接口
Service stopped|服务已停止
Setup needed|需要完成配置
Inactive hours|休息时段
Preview mode|预览模式
Agent active|机器人活跃中
AI participation is paused until the next active window.|AI 参与已暂停，将在下个活跃时段恢复。
The agent is ready to participate in enabled conversations.|机器人已准备好参与启用的聊天。
SETTINGS APPLIED|配置已应用
SERVICE STOPPED|服务已停止
APPLYING SETTINGS|正在应用配置
Saved settings are active|已保存配置已生效
Saved. Start the service to apply.|已保存，启动服务后应用。
Waiting for the agent to apply settings…|等待机器人应用配置…
Reconnecting…|正在重连…
Connection interrupted. Retrying automatically.|连接中断，正在自动重试。
Settings saved. The agent is applying them now.|配置已保存，机器人正在应用。
Loaded saved settings.|已载入保存的配置。
Testing the saved model configuration…|正在测试已保存的模型配置…
Groups|群聊
Private contacts|私聊联系人
No contacts returned.|未返回联系人。
Loading models from the saved provider…|正在从已保存的服务加载模型…
Fix the advanced JSON before saving the selected model.|请修复高级 JSON 配置后再保存所选模型。
Sending to the connected account…|正在向已连接账号自己发送…
Incorrect access key|访问密钥不正确
Sign in required|请先登录
Request failed|请求失败
Session verification failed; sign in again.|会话验证失败，请重新登录。
Too many attempts. Try again in ten minutes.|尝试次数过多，请十分钟后重试。
Settings changed elsewhere. Reload before saving.|配置已被其他会话修改，请重新载入后再保存。
Save an API key and model first|请先保存 API 密钥和模型。
API authentication and JSON response verified. No QQ message sent.|API 身份验证和 JSON 响应均正常，未发送 QQ 消息。
No matching events yet.|暂无匹配事件。
No events captured. If the test finishes empty, check self-message reporting and the bridge event connection.|尚未收到事件。若测试结束仍为空，请检查自身消息上报和桥接事件连接。
`;
const zh = new Map(pairs.trim().split('\n').map(line => line.split('|')));
let language = 'zh-CN', observer;
const originals = new WeakMap(), attributes = new WeakMap();
export function translate(text, locale = language) {
  if (locale !== 'zh-CN') return text;
  if (zh.has(text)) return zh.get(text);
  if (/^\d+ active conversations$/.test(text)) return text.replace(' active conversations', ' 个活跃聊天');
  if (/^Account \d+$/.test(text)) return text.replace('Account ', '账号 ');
  if (text.startsWith('UPDATED ')) return text.replace('UPDATED ', '更新于 ');
  if (text.startsWith('Complete setup: ')) return text.replace('Complete setup: ', '请完成配置：').replace('API key', 'API 密钥').replace('model', '模型').replace('selected chat IDs', '启用聊天');
  if (text.startsWith('Service command completed: ')) return text.replace('Service command completed: ', '服务操作已完成：');
  if (/^\d+ models returned\./.test(text)) return text.replace(/ models returned\..*/, ' 个模型已加载。选择后保存应用，也可手动填写。');
  if (/^(idle|connecting|listening|stopped|finished|failed) ·/.test(text)) return text.replace(/^(idle|connecting|listening|stopped|finished|failed)/, state => ({ idle: '未开始', connecting: '正在连接', listening: '监听中', stopped: '已停止', finished: '已结束', failed: '失败' })[state]).replace(/(\d+) events/, '$1 条事件').replace(/(\d+)s remaining/, '剩余 $1 秒');
  if (text.endsWith('. You can enter the model ID manually below.')) return text.replace('. You can enter the model ID manually below.', '。你可以在下方手动填写模型 ID。');
  if (text.endsWith('. Not retried. Check QQ before trying again.')) return text.replace('. Not retried. Check QQ before trying again.', '。未自动重试，请先在 QQ 中检查是否收到消息。');
  return text;
}
function render() {
  observer?.disconnect();
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node; (node = walker.nextNode());) {
    if (node.parentElement?.closest('script,style,textarea,code,#log-output,#debug-events,#thought-list,#contacts,#decision-list,#model-choice,#model-value,#advanced-json') && !node.parentElement?.matches('.empty, #model-choice > option[value=""]')) continue;
    const current = node.nodeValue, prev = originals.get(node);
    const source = prev && current === prev.rendered ? prev.source : current;
    const rendered = source.replace(/\S[\s\S]*\S|\S/, value => translate(value));
    originals.set(node, { source, rendered });
    if (current !== rendered) node.nodeValue = rendered;
  }
  for (const el of document.querySelectorAll('[placeholder], [aria-label]')) {
    const saved = attributes.get(el) || {};
    for (const name of ['placeholder', 'aria-label']) {
      if (!el.hasAttribute(name)) continue;
      const value = el.getAttribute(name), prev = saved[name];
      const source = prev && value === prev.rendered ? prev.source : value;
      const rendered = translate(source); saved[name] = { source, rendered };
      if (rendered !== value) el.setAttribute(name, rendered);
    }
    attributes.set(el, saved);
  }
  document.documentElement.lang = language;
  document.title = language === 'zh-CN' ? 'Luma · QQ 机器人控制台' : 'Luma · Agent console';
  observer?.observe(document.body, { childList: true, subtree: true, characterData: true });
}
export function setLanguage(value) {
  language = value === 'en' ? 'en' : 'zh-CN';
  try { localStorage.setItem('qq-ui-language', language); } catch {}
  if (typeof document !== 'undefined') render();
}
export function startI18n() {
  try { language = localStorage.getItem('qq-ui-language') === 'en' ? 'en' : 'zh-CN'; } catch {}
  document.getElementById('interface-language').value = language;
  observer = new MutationObserver(render); render();
}
