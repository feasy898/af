# 第 0 档直接采纳 · 落地台账（fix4-align2-tier0）

> 2026-10-22，改进实施员（对齐组第 2 循环 · tier0 采纳轮）。
> 清单来源（逐条回溯原文，非转述）：`harness策略调研/决策简报.md` §二「第 0 档：直接采纳」12 族
> + §三 前 3 项零 LLM 成本纯收益项（S002/S083/S118）
> + `strategies.csv` 测试设计列标注「直接采纳」的 S156/S160 ——合计 **17 族**。
> 基线：合流 `fix4-align2@0dd1a23`（scaffold v6 = v4∪v5，见该提交信息）。
> 纪律：纯采纳不动测量语义（判分链/超时/判分参数零触碰）；每项落地带原文出处与代码锚点；
> 与现树冲突或不适用的**如实跳过并记录原因**（见「跳过/已内建」栏）。

## 一、17 族处置总表

| # | ID | 一句话机制（原文锚点） | 处置 | 落点 |
|---|---|---|---|---|
| 1 | S128 | 模型改文件、runner 从 git 状态导出 patch（19.3%→71.2%） | **已内建**（无需改动） | `crates/af-bench/src/swev.rs` `capture_diff`：`git -c core.fileMode=false diff` 从容器 git 状态导出 agent.diff，判分与完成度门禁共用；模型从不手写 diff |
| 2 | S077 | SEARCH/REPLACE 多级容错解析+结构化错误反馈 | **跳过（不适用）** | af 无编辑块解析层：模型经 bash 工具直接改文件，不存在 SEARCH/REPLACE 格式面；该族针对的「格式性编辑失败」在 af 的失败面上无对应物 |
| 3 | S066 | 代码正文不用 JSON 承载，用 markdown/文本块 | **已内建**（无需改动） | 代码正文经 bash 工具（纯文本命令/heredoc）进容器（`crates/af-bench/src/tb.rs` `bash_tool_decl`），无 JSON 承载代码的通道 |
| 4 | S065 | 编辑格式按模型路由（weak=whole/strong=diff） | **跳过（不适用）** | 单一模型档案（mavis-m31f）且无编辑格式层，路由无对象；若后续扩模型档案再启用 |
| 5 | S033 | 治理/安全约束钉在压缩之外（违规率 30%→0%） | **已内建**（无需改动） | af 无有损压缩器：system prompt 每轮重发（`crates/af-harness/src/run.rs` 请求组装处 push System）；r2 离线声明三通道（system/工具描述/instance 模板）；S002 折叠明文排除 System/User 项 |
| 6 | S032 | 压缩摘要必须来源校验（83% 失败含臆测） | **跳过（不适用）** | 现树无 LLM 摘要压缩器（S009 负面结论已采纳：不引入）；S002/S118 折叠为确定性机制，无「臆测条目」面 |
| 7 | S073/S074/S045 | 沙箱底座：任务级容器+默认断网+白名单；FS/网络双边界；镜像分层复用 | **已内建**（容器形态） | 每任务一次性容器（`tb.rs` `TbEnv::setup`：先 `docker rm -f` 再 `docker run -d`）；容器级网络限制+离线三通道声明（v5）；资源护栏 `--memory/--pids-limit`（tb.rs 常量+单测固化）；镜像复用=sweb.eval 预拉镜像（r1 探针基建事故反证镜像即复用单元）。S074 的 OS 级双边界由容器边界覆盖其威胁模型，bubblewrap 形态在 docker 宿主面不适用 |
| 8 | S130 | harness 版本级回归防护（35 release 波动 23-39%） | **采纳（本轮运营化）** | 固定回归集协议（见 §二）；本轮 5 题回归快测即首个执行实例 |
| 9 | S147/S148/S149 | 饱和集回归+活水外推；截止时间切片；基准退役机制 | **采纳（协议文档）** | §三：回归集=已过题清洁判池；活水=新题池（SWE-bench-Live 方向）；退役阈值=SOTA 两季涨幅<3pp 或审计缺陷率>25% 降级为回归集 |
| 10 | S150/S151 | 置信区间+配对差分析；小差距不可排序+溯源 | **采纳（代码+协议）** | S151 代码化：结果行 `harness_commit` 字段（本次 commit 3a548c1，五处结果行）；per-instance 矩阵=results.jsonl 逐题行；统计纪律见 §四 |
| 11 | S152/S170 | 成本归一化指标（同等成本解决率为一等指标） | **采纳（协议文档）** | 结果行已带 `tokens{input,output,cache_read}`/`turns`/`agent_wall_ms`；报告口径补 $/resolved 与失败÷成功成本比（§四） |
| 12 | S156 | 三层 evals+失败模式正则分类（pass@k/flakiness） | **采纳（协议文档）** | §五：L1=cargo test 单测、L2=1 题冒烟、L3=bench 批次；失败先归因（setup_failed/infra vs not_resolved，gate+verifier_note 字段）再比策略；pass@k/pass^k/flakiness 留多试验轮启用 |
| 13 | S169 | Batch API 半价跑大批评测 | **采纳（政策记录）** | §六：时效允许的大批次评测走 Batch API；**现 endpoint 无 batch 支持**，落地前为政策记录，不虚报已实现 |
| 14 | S002 | 工具结果清理/上下文编辑（+29%，token −84%） | **采纳→回归回退** | commit 9818633 落地（`Policy.observation_window` + `run.rs::fold_observations`）；采纳轮 5 题回归门禁 2/5（要求全过）→按「掉过→回退定位」回退（scaffold v8）。头号嫌疑腿=mpl-25775（净对照：同墙钟无折叠 2/2 过 vs 本臂 0/3，见 §二执行记录）；4094 为墙钟边缘竞态非归因腿。机制保留于历史 commit，待专轮调参（窗口大小/触发条件）A/B 后重测 |
| 15 | S083 | 大工具输出外置（>20KiB 只回路径+预览） | **代码采纳（保留）** | commit bb018d3：`tb.rs exec` 超 12KiB 完整输出写回容器 `/tmp/.af-outputs/`（base64 分块），上下文只进预览+路径+回读指引；失败退有损截断 |
| 16 | S118 | 只保留最近 5 条观测全文（+3.0pp，官方默认） | **采纳→回归回退** | 与 S002 同一机制（commit 9818633），随折叠一并回退；机理侧写（可复算口径，2026-10-23 订正）：重复区域重读 臂 6/8/24 vs 对照 0/0；触 text.py（`effects.db` effect_intent）对照 26/27 vs 臂 93/105/118——keep-last-5 对 af 的长程修题轨迹过激（旧输出回读路径被折叠改写→重读失控），与 SWE-agent 编辑器形态差异待 bisect/专轮核实 |
| 17 | S160 | 技能边际收益先证伪（49 技能 39 零提升） | **跳过（不适用）** | af 无技能注入面；该族是「引入技能库前的准入门槛」，待有技能面时按其配对开/关协议执行 |

## 二、S130 版本级回归协议（本轮起生效）

- **回归集**：从「真实差距池之外」的已过题（`/tmp/truth-rejudge` 清洁判 reward=true）中抽 5 题，
  固定模型档案（mavis-m31f）+ `--judge-mode clean`。
- **门槛**：5 题应全过；任一题掉过 = 本次采纳引入回归，须回退定位（`git bisect` 本分支 commit 序列）。
- **双曲线**：同报 resolved 与 tokens/turns（S130：resolve rate 与 token 消耗必须一起看）。
- **溯源**：结果行 `harness_commit` + `scaffold` 双锚定（S151）。
- **本分支执行记录**（2026-10-22 采纳轮；2026-10-23 依净对照订正归因）：scaffold v7（fold+S083）
  批次 2/5——xarray-4094 F（wall@1800s）、requests-1766 F（completed）、mpl-25775 F（wall@1800s）、
  mpl-25122 T、xarray-3677 T ⇒ **回归成立（2/5）**。
  **归因（净对照实验闭合至 tier0 层）**：与 v7 臂同面 1800s 墙钟、无折叠的净对照（0dd1a23 内容）复跑
  mpl-25775 **2/2 过**（a1 completed@505s；a2 被 wall 硬切仍因补丁 10143B 在位判 T）vs v7 臂同题
  **0/3 全 wall_timeout 且切时 agent.diff=0B** ⇒ 25775 臂败归 tier0 层，**S002/S118 观测折叠为头号
  嫌疑**（对照 51/61 轮 vs 臂 153/172/225 轮，`effects.db` 可复算口径：触 text.py 对照 26/27 vs 臂
  93/105/118），S083 与折叠的最终分格待 9818633 前后 bisect。**4094 属墙钟边缘竞态**（本臂自身
  2/3：T/T/F，结局取决于补丁是否在 ~1800s 前落树；第二循环所称「v6 对照臂 T」实为 v3 代名实不符，
  不构成 4094 归因腿）；1766 在无折叠无墙钟对照臂亦 F（completed），非本轮采纳引入。
  **限定**：净对照 a2 仍被墙钟硬切——墙钟作为一般约束未被免除，**不得写「墙钟单因可解释 25775」**。
  回退后 scaffold **v8**（= v7 − 折叠；S083/墙钟 1800s/S151 持恒）= 折叠摘除后的隔离臂。其 5 题
  门禁重跑在本 commit 时点**尚未兑现**；判读规则（带净对照参照臂）：v8 过 25775 ⇒ 折叠致；败 ⇒
  折叠洗清、嫌疑转 S083/运行方差；混合结局按题分判；per-task 聚合 k=2 中 1 次 no_patch 切断 ≠ 题败。

## 三、基准组合与退役机制（S147/S148/S149）

- 回归集：`/tmp/swev-s1` 已过题子集（清洁判 T），固定快照，只做回归不做排名。
- 活水集：新题池按入库时间标记（S148 时间切片），主指标只取模型截止时间之后的题目子集。
- 退役机制：固定集连续两季 SOTA 涨幅 <3pp 或审计缺陷率 >25% → 降级为回归集（S149 方向性结论，
  其具体数字未核实不外引）。

## 四、统计与成本纪律（S150/S151/S152/S170）

- 任何 harness 对比报 per-instance 通过矩阵（results.jsonl）+ 配对差分析；CI 不含 0 才可宣布改进。
- 小差距不可排序：报告「分辨实例数」（两版本结果不同的任务数）而非只报总分。
- 成本与通过率同报：$/resolved、失败÷成功成本比（tokens 与 agent_wall_ms 结果行已有，价格表按日戳维护）。

## 五、三层 evals 与失败归因（S156）

- L1 合同测试：`cargo test`（判分链/协议/折叠/外置的纯函数与集成单测）。
- L2 冒烟：每轮采纳后 1 题 clean 快测（本轮执行）。
- L3 E2E：bench 批次（本轮 5 题回归）。
- 失败先归因后比较：`setup_failed`/`run_failed`（基建）与 `not_resolved`（策略）分桶；
  gate 字段与 verifier_note 留正则分类的挂点（failures.yaml 式，待失败样本积累后建表）。

## 六、成本政策（S169）

大批次、时效不敏感的评测负载优先走 Batch API（厂商口径 5 折，数字未文档级核实，
以账单自验为准）。现 endpoint（af-harness/src/endpoint.rs）尚无 batch 通道——
政策先行，落地另立工单；交互式探针/冒烟不适用（24h 窗口约束）。

## 七、红线自查（本轮全部落地项）

- 判分链零改动：三个代码 commit 的 diff 不含 `tools/swev-judge.py`、判分参数、超时口径
  （`git diff 0dd1a23..HEAD --stat` 可复核：仅 tb.rs/swev.rs/main.rs/spec.rs/run.rs/tests.rs/smoke.rs/docs）。
- 零未来信息：S002/S118 折叠只动「已有对话里模型自己产生的旧工具输出」；S083 只外置本 run 的输出；
  无任何上游/历史/不可达对象引导文案（ARCHAEOLOGY_PATTERNS 原样）。
- 测量语义：结果行的 reward/status/judge_mode 口径零变化；新增字段仅 `harness_commit`（元数据）。
