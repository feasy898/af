# C1（候选②位）失败后转向纪律 · 实施说明与 A/B 预注册

> 分支 `fix4-align4-p1`（worktree `~/workspace/af-wt-align4-p1`）｜基线 `50ba8a8`（= `fix4-align2-v8`，已提交态）｜写作时点 2026-10-23 07:1x–07:2x CST｜候选实施员乙（并行路 B）
> 来源：`计划/研究组/对齐组-state.md:948`（§25.1 候选台账 C1 行）；设计条件 §25.2（:954-956）；红队门审 node **32972 confirmed**（5 条 issue，原文 §25.3 :965）；落地要求 §27.3 :1021 六项。
> B2 来源件：**③纠错风格** 可迁移件的 af 原生化（件号订正见 §2-②）。

---

## 1. 机制与注入面（实现摘要）

- **条款**（`crates/af-bench/src/main.rs` 常量 `SWEV_REDIRECT_DISCIPLINE`，英文与既有协议一致）：
  1. 命令失败（非零退出 / is_error）是证据：先**一行**写明该失败改变了你对问题的哪点理解，再发出一条**实际不同的**命令（不同旗标/输入/目标/路线）。
  2. 无变化时不逐字重跑失败命令（同输入同输出）；确有实质变化（改码后重跑同测试）可重跑——同样一行写明变了什么（**必要重试通道保留**，对应落地要求⑤）。
  3. 不道歉、不反刍；一行诊断后直接下一步。
  4. `exit=141`（SIGPIPE，多见于 `| head`/`| tail` 截断）：写文件再回读，不以同型管道重试（**依赖登记**见 §2-⑥：本代惰性）。
- **注入点**：swe-v 线 `user_message` 拼装处唯一注入（`swev_instance_protocol()`，原 `format!("{}\n\n{}", task.prompt(), SWEV_INSTANCE_PROTOCOL)` 改经旋钮拼装）。on = 协议原文 ∪ 条款（**只增不改**）；off = 协议原文**逐字节不变**（单测钉死）。
- **system prompt 零改动**：两侧共用同一常量 `SWEV_SYSTEM_PROMPT`（一行，af 分工，B2 回灌移交单 §四形态）。
- **TB / InjecAgent 口径不动**：`main.rs` 三处 `ContextTemplate` 中仅 swe-v（:947/:990 区）被改；terminal-bench（:1362 区）与 injecagent（:1540 区）零接触（B2 §四「全局改动连带处置」要求已核）。
- **旋钮**：`--redirect-discipline <on|off>`，缺省 **on**；非 on/off 值报错（不静默落缺省）。**同二进制 on/off 配对**（红队 32972 issue ④ / 三审 32984 issue ⑥ 强制条件；候选①「单臂无配对」教训同构，§25.2）。
- **scaffold v8→v9 仅作锚定**：两处结果行 `scaffold: "v9"` 锚代际；臂态由结果行新字段 **`redirect_discipline: true|false`** 自述（p00 墙钟旋钮同型）——杜绝「scaffold bump 把文案效应与代际差混装」。
- **红线核验**：判分路径（`swev.rs` 判分链、gate 语义、`tools/swev-judge.py`）零改动；测量语义零改动（结果行仅增自述字段）；未来信息零引入（条款不含任何 test-patch / 隐藏规格信息，单测断言在案）；gov 生产面零触碰；生产位二进制未动。

## 2. 落地六项对照（对齐组-state.md §27.3 :1021 C1 行 → 本实施）

| # | 要求 | 落实 |
|---|---|---|
| ① | **补既有「逐字重复命令」护栏的重叠分析**（v8 树 `main.rs:936` 附近：recent 窗口 6 条、同命令窗口内 ≥2 前科即注入提醒；4094 三跑触发 0 次、35/12/7 次重复落在盲区 ⇒ 增量通道为真），写进实施说明 | ✅ 本节 §3：护栏机制代码锚点 + **本节点亲算复算**（此前为 node 32972 复算、§28.5 列为未复算项）双口径坐实「盲区为真、增量通道为真」。结论已同步写入 `SWEV_REDIRECT_DISCIPLINE` 常量文档注释与结果行注释 |
| ② | **订正 B2 件号与撤证落点** | ✅ C1 的 B2 来源件号为 **「③纠错风格」**（红队 32972 issue ② 订正；非「②」）。撤证事实：`计划/研究组/调优组-state.md:1110`（三审 32898 issue 2 材料性：B2 回灌评估经验腿不可复现且被 8/25 反例否证 ⇒ **「本题型真正起作用的差是人格/收尾/纠错风格而非交付格式」这句推断失去证据支撑，须删**）+ `计划/研究组/B2回灌-移交①对齐组-20261023.md` §一 P1（af_equivalent=partial、旋钮语义在 af 判分链下结构性惰性）与 §三.3.3（推断句删除令）。**C1 立论不依赖该已撤证推断**：C1 的成立依据是 af 自身轨迹证据（4094 型「215 轮膨胀+切断时无补丁」打转签名，state.md §13.2 issue 1/7），mcode 侧文案源仅作措辞参考（B2 移交单 §二差异清单：「Correct mistakes briefly」→「Correct yourself when an error changes the user's decision…don't over-apologize or ruminate」） |
| ③ | **r1「424 事件/0 is_error」标注** | ✅ 引用时一律标注：**「ledger 亲验（第一循环记账员 node 32481 扫 r1 五判分行 run_dir/normal.jsonl）、源目录已清不可复跑」**——本节点 07:2x 亲查：`/tmp/align-probe-r1/` 已不存在（仅余 `-r2/-r3/-r4` 重试批目录与 `.log`），该计数当期**不可复算**（红队 32972 issue ③）。本实施说明不复述该数为新证据，仅作机制史注：r1(v4) 代无 is_error 负反馈通道，r2 起失败可见（p01 台账 260 事件/62 is_error）——C1 的条款作用于失败**语义吸收**层，与 is_error 通道互补而非重复 |
| ④ | **A/B 主读数含轨迹量** | ✅ §4 预注册：主判定=题目级通过集合；**主读数=turns / agent_wall_ms / 逐字重复率 / tool_free_streak 四轨迹量**（口径定义见 §4），reward 不作唯一读数 |
| ⑤ | **监测必要重试率** | ✅ §4-④ 预注册指标与告警线。依据 `对齐组-state.md:209`（原文逐字）：「实测 fix1 m31f 有 **29 个 run 存在 ≥3 次连续非零退出**，其中有清洁判的 25 个里 **19 个判 T**（astropy-13579、django-10880、xarray-4075、requests-1142 等）→ 朴素实现会**歼灭约 19 个通过用例**」⇒「不逐字重跑」若被模型读成「禁止重试」会伤通过集。对策双轨：(a) 条款第 2 条明文保留「实质变化后的重跑」通道；(b) A/B 两臂同表监测必要重试签名（§4-④），ON 臂判 T 集相对 OFF 臂的收缩若与重试压制同现，判读为**必要重试被压制**（负向信号，不得记为「更专注」） |
| ⑥ | **与候选③（pipefail）的依赖关系** | ✅ 本二进制（v9）**不含 pipefail**（`strings` 代际标记核验见 §5）：SIGPIPE 被末端 `tail/head` 的 rc=0 掩蔽，模型几乎见不到 `exit=141` ⇒ 条款第 4 条在本代**惰性但无害**（前瞻兼容）。登记三条：(a) 本代 A/B **不得**把 141 子句收益记入 C1 读数；(b) 若 C3 同批过门，141 子句收益读法**分列臂**（C3-only vs C3+C1-on，同法配对）**或**按本节预注册联合读法：「141 子句收益仅在 pipefail 在场代际可判，且须以同批 `scaffold`/结果行自锚的 pipefail 在场性为前提，与 C3 自身收益分列归因」；(c) 两候选若同批跑批，禁止共用同一对照臂做两条增益主张（候选①③重叠核 node 32816 §F 纪律沿用） |

## 3. 落地要求①实证：既有护栏重叠分析（本节点亲算，as-of 2026-10-23 07:2x）

**既有护栏**（v8 树 `crates/af-bench/src/main.rs` run_swev bash 闭包内，注入文案行 :936 附近）：每次 bash 调用时，`recent` 缓冲保存**最近 6 条**命令；若当前命令在窗口内已有 **≥2 次**前科 → 在工具输出前注入 `[harness] You have issued this exact command N times. Repeating it will not make progress…`。抓的是**近距离连发**（窗口 6 条内第 3 次及以上出现）。

**本节点复算命令与输出**（`effects.db` → `effect_intent.payload_json` 的 `arguments.command` 逐 seq 重建命令序列；护栏按源码语义仿真：计数窗口=最近 6 条先前命令、前科 ≥2 即注入；并以 `normal.jsonl` 与 `harness.db.transcript_json` 全文检索注入文案为直接证据）：

| 4094 跑 | bash 调用 | 逐字重复实例 | 单命令最大出现 | 护栏仿真触发 | 注入文案命中（normal.jsonl / transcript） |
|---|---|---|---|---|---|
| tier0 regress a1（原败，F wall_timeout 215轮） | 219 | **35** | 8×（`cd /testbed && grep -n "def to_unstacked_dataset" …`） | **0** | **0 / 0** |
| tier0 k2 a1（T completed 154轮） | 170 | **12** | 4×（同上 grep） | **0** | **0 / 0** |
| tier0 k2 a2（T wall_timeout 142轮） | 163 | **7** | 4×（`cd /testbed && grep -n "def to_stacked_array" …`） | **0** | **0 / 0** |

- 逐字重复的**相邻间隔**全部 > 6（原败跑 top 命令间隔 26/27/29/15/11/7/9；k2 两跑 7/87/28 与 22/61/21）⇒ **35/12/7 次重复全部落在 6 条窗口的结构盲区**，护栏从未触发——**与红队 32972 issue ① 的「触发 0 次（35/12/7 落盲区）」逐位一致**（该数此前为节点自报，§28.5 列为未复算项；本节点以仿真 + 台账双口径独立坐实）。
- **增量通道为真**：C1 条款覆盖的是**决策层**通道（失败语义一行吸收 + 强制转向 + 反刍压制），作用面在模型决策，不依赖命令在窗口内近距离连发；与既有输出层护栏**互补不重复**。4094 型「长间隔同命令反复重查 + 215 轮膨胀 + 切断无补丁」恰是条款的目标失败签名。
- 盲区注记（诚实披露）：本条款为提示词层引导，**无硬止损**；既有护栏对窗口内近距离连发仍保留硬注入，两层并存。

## 4. A/B 预注册（同二进制 on/off 配对；探针执行节点照此跑，勿临场改口径）

- **二进制**：`/home/pan-ding/afbuild-shared/af-bench-fix4-align4-p1`（sha256 见 §5；构建与测试证据见 §5）。
- **两臂命令**（长跑 `setsid nohup`；题集 >2 拆 ≥2 进程——题级并行安全，因不同题容器名不同）：
  ```bash
  BIN=/home/pan-ding/afbuild-shared/af-bench-fix4-align4-p1
  # ON 臂（条款注入；缺省即 on，旗标显式写出便于审计）
  setsid nohup $BIN --suite swe-v --data-root /tmp/swev-s1 --profile mavis-m31f \
    --tasks <题集> --out /tmp/align4-p1-c1on --host anolis --judge-mode clean \
    --redirect-discipline on > /tmp/align4-p1-c1on.log 2>&1 &
  # OFF 臂（对照；user_message 逐字节=v8）
  setsid nohup $BIN --suite swe-v --data-root /tmp/swev-s1 --profile mavis-m31f \
    --tasks <题集> --out /tmp/align4-p1-c1off --host anolis --judge-mode clean \
    --redirect-discipline off > /tmp/align4-p1-c1off.log 2>&1 &
  ```
- **同题跨臂隔离（硬纪律）**：容器名生成式无臂标识（`swev.rs:57-59` = `afbench-swev-<task>-<profile>`，第三循环撞名事故 §22.4）⇒ **两臂串行**（ON 完再 OFF），或换 `fix4-container-names` @ `daef39c` 分支二进制（本 A/B **不换**，保持单变量）；若因调度必须两臂并发，则改 profile 别名（p01 形态：`mavis-m31f` 增别名，端点/模型/key/提示词全同，仅容器名后缀），并披露。起批前 `ssh anolis 'docker ps -q | wc -l'` <12 且本组 ≤4；镜像检查 `docker images | grep sweb.eval`（**禁 `docker images -q`**）；事后逐行核对结果行 `redirect_discipline` 字段与 out 目录。
- **题集**：须含 C1 target_failure 签名题 **`pydata__xarray-4094`**（错误触发型打转）；建议并表 `matplotlib__matplotlib-25775`（同表可比 C2 签名）+ 差距池现算题补足 5 题（**池现算现引**，§27.4⑨；禁用 `/tmp/gaptasks.json`）；终选题集由探针节点锁定后随结果披露。k≥2 时 per-task 以**多数/全 T 为过**（红队 32980 issue ④ 口径借用）。
- **判分**：`judge_mode=clean`、`attempts=1`、零判分参数改动；结果表逐行标出处（out 目录+行号），基建失败行与模型败行分桶（§19.2⑨）。
- **读数（预注册）**：
  1. **主判定**：题目级通过集合（每题 reward 两臂逐题对照；n=5 单样本，聚合值不作判读——首循环 4/5 翻转教训 §4.3）。
  2. **主读数轨迹量（不只 reward）**：
     - `turns` / `agent_wall_ms`（结果行现成字段）；
     - **逐字重复率** = 逐字重复实例数 ÷ bash 调用总数（per run；口径=本说明 §3 复算口径，`effects.db` `effect_intent.payload_json` 逐 call）；
     - **tool_free_streak** = 该 run 内**最长连续无工具调用轮数**（口径：`harness.db` `transcript_json` 连续 assistant 轮无 tool_use；与 `tool_free_turn_limit=3` 终态对读）。
  3. **completed-but-F 同表监测**（基线=订正后 **8/27**：清洁判 89 行中 27 行 F、其中终态 completed 8，红队 32968 issue ②；**禁写 8/148**）。
  4. **必要重试率**（落地要求⑤）：`必要重试签名 run` = 存在 ≥3 次连续非零退出（口径：`effects.db` receipt 的 rc 序列，或 `normal.jsonl` `is_error`+`error_code` 前缀 `exit:` 连续 ≥3）且该 run 判 T；监测两臂「该签名 run 数 ÷ 判 T run 数」。**告警线**：ON 臂判 T 集相对 OFF 臂收缩、且收缩题在 ON 臂呈现重试压制签名（失败后不再重试即转线）⇒ 判读为负向信号「必要重试被压制」，**不得**记为正向「更专注」。
  5. **141 子句**：本代不计读数（§2-⑥）。

## 5. 构建、测试与产物（本节点亲跑，as-of 2026-10-23 07:1x–07:2x）

- **基线链**：`50ba8a8`（`fix4-align2-v8`，本循环 v8 采纳员已提交——本节点 07:1x 亲见其从 staged 中间态落为提交，分支名 `fix4-align2-v8`）→ 本分支唯一增量 commit（C1）。若 v8 采纳员最终反悔（`revert --abort`），本分支按 ask 预案 **rebase 到 `0dd1a23` 内容基线**并如实登记（rebase 面：仅 `main.rs` 拼装/scaffold 区；v8 的 `spec.rs/run.rs/tests.rs` 增量不在本分支独立存在，随基线自动消失）。
- **它线在飞登记**：track A（`fix4-align4-p0` @ `b1bd179`，候选①位 C2 实施员甲）07:19–07:2x 在共享 target `~/afbuild-shared` 跑 clippy/check——本实施员**等待其退出后**才执行清理+构建（纪律④在飞不动），其产物 `af-bench-fix4-align4-p0`（07:21）零接触；longcat 线（`af-bench-longcat`）在飞零接触。另观察到 v8 产物实际落 `~/afbuild-shared/af-bench-fix4-align2-v8`（07:14，非 §27.2 V4 预写的 `~/afbuild-align2/` 路径）——**仅登记为观察到的事实**，处置归 v8 采纳员。
- 共享工件病防护（三循环教训 + §19.2⑦）：确认无在飞 cargo 后执行 `cargo clean -p af-bench -p af-harness -p af-stop`（**Removed 3313 files, 2.5GiB total**）再编；`build_ok` 以 **`cargo test` 编译+通过**为据（不以 warm `Finished` 为据）。
- **测试结果（本节点亲跑）**：`cargo test -p af-bench -p af-harness -p af-stop` → **128 passed / 0 failed**（af-bench **17** = v8 基线 15 + 本分支新增 2（旋钮拼装/取值 + system·判分面隔离）；af-harness lib 57；af-stop 33；properties 7；restart 13；doc 1）。`cargo fmt --check` 干净；`cargo clippy -p af-bench` 无新告警（仅依赖既有 future-incompat 提示 proc-macro-error2，与基线构建同）。
- **产物**：`/home/pan-ding/afbuild-shared/af-bench-fix4-align4-p1`，sha256 `73390f93662afe5c89df8bc78209319a16481c8c5cdc4d0964ba7cc498ca457a`（07:23，**新名，未覆盖任何既有产物**；生产位未动）。
- **代际标记自证（`strings -a` 亲跑）**：`--redirect-discipline`=2（C1 旋钮在场）；`--wall-timeout-secs`=0（候选①不在，本分支基线无 p00 增量）；`pipefail`=0（候选③不在 ⇒ §2-⑥ 的惰性登记为事实）；`observation_window`=0、`fold_observations`=0（v8 内容）；条款文案（`state in one line what this failure changed`）=1、既有护栏文案（`You have issued this exact command`）=1（两层通道并存）；结果行字段名 `redirect_discipline`=5。旋钮冒烟：`--redirect-discipline maybe` → `af-bench failed: --redirect-discipline 只接受 on/off（得 maybe）`（拒绝非法值，不静默落缺省）。

## 6. 明确未做的事

- **未跑**任何判分/探针/容器批（A/B 属探针节点；本说明仅预注册）。
- **未动** docs/tier0-adoption.md（其 V1 订正属 v8 采纳员采纳序①，本分支原样继承 `50ba8a8` 的该文件）。
- **未裁决**题集终选、A/B 排期与它线让行时序（探针节点/运营者裁决）。
