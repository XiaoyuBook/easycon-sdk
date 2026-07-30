# 0019：冻结 Phase 4 C1 lexer 合同

- 状态：Accepted / Frozen C1 Lexer Contract
- 提议日期：2026-07-31
- 接受与冻结日期：2026-07-31
- 固定起点：`ecbd39f4fc89a4a0c3768b0689e7ae495c29d664`
- 初始 proposal：`8256fde2b760899c36cc1efa79e68a3f3a88e6ba`，tree
  `84027d071471be3ff9f4eee15caf1d889f3bc194`，parent
  `ecbd39f4fc89a4a0c3768b0689e7ae495c29d664`
- 固定接受候选 / 修订：`e4b12b5fa9569df51b371c5bd9cf0b5cf38c8553`，tree
  `3dcefcda750986aff9d71462292e7b438380b5f8`，parent
  `8256fde2b760899c36cc1efa79e68a3f3a88e6ba`
- 最终独立 full review：任务 `019fb40a-ef77-7f12-8075-885be6a0e917`，结论 `APPROVE`，
  P0/P1/P2=`0/0/0`
- 上位目标：[ADR-0017](0017-phase-4-ecs-automation-target.md) 的
  `Accepted / Frozen Phase 4 ECS/Automation Target`
- S0 证据：[ECS provenance manifest](../../spec/fixtures/ecs/manifest.json) 与
  [fixture generator](../../tools/generate_ecs_provenance_fixtures.py)
- 只读裁定：任务 `019fb3c0-17dd-7eb0-a00a-ee09e881049d`
- 编号说明：ADR-0018 已被 Controller D0 占用，本 ADR 使用 0019

## 状态、生效条件与范围

固定接受候选 `e4b12b5fa9569df51b371c5bd9cf0b5cf38c8553` 已由独立 reviewer 对固定对象完成只读 full review，
结论为 `APPROVE`，P0/P1/P2=`0/0/0`。本 ADR 现接受并冻结该候选的 C1 lexer 合同，并授权后续 C1 implementation
节点按本文的 RED 顺序启动。本次 docs-only acceptance 只改变状态、治理链和审查证据，不修改 reviewer 已批准的
token/value/span、恢复、diagnostic、ownership 或 conformance 合同。

本 acceptance 不冻结或声明任何 C1 实现，不表示 Rust、Workspace、fixture reproduction、conformance、CI、硬件或
发布已经通过。C1 仍须形成独立实现提交，完成本文要求的 exact tests、完整门禁与 fixed-SHA implementation review；
这些证据不能由 proposal 或 acceptance 的 docs-only 门禁替代。任何后续语义修订都必须形成新的固定候选并按受影响
冻结面重新接受独立审查与单独 refreeze。候选和本 acceptance 的自身 SHA/tree 只由提交后的 Git 对象、固定 ref 与
外部结构化报告记录，不在 tracked 文件中预言。

本合同只补齐 ADR-0017 未唯一回答的 lexer 决策，不修改其已经冻结的 ownership、ProgramHash、单个 leading BOM
处理、LF/CRLF/CR、UTF-8 byte span、statement/expression builtin 分层或 user-symbol case 合同。它不修改 S0 exact
source/hash/profile/expected，不修改 `EasyCon/`，也不扩展 parser、binder、lowerer、evaluator、port、Runtime、ABI
或 public API。当前 W0/S0 已完成，R0 尚未完成独立 review/refreeze；Controller D0 的 ADR-0018 已接受，但
D0-D2 与 C1 相互独立。

## 证据与待决缺口

ADR-0017 已冻结 PRINT continuation 的最终 `PrintFragment` 语义，以及 BOM/newline/span 和大小写分层；S0 又冻结了
五份 provenance 独立、bytes 相同的 42-byte PRINT source及其 ProgramHash 和最终 expected fragments。它们没有冻结
raw-tail 的 token/value/span、普通字符串 escape 集合、mixed-case 普通 keyword/boolean，或 invalid token 的精确恢复。
因此 S0 source 不能被修改成另一种写法来掩盖缺口，S0 的最终 fragment 也不能反推成已经通过的 C1 token trace。

固定 legacy 事实只解释缺口来源：statement-head PRINT/ALERT 使用 raw-tail，而普通 quoted string 使用另一条
escape 路径。下文是 v1 产品合同，不把 legacy 的 span、错误恢复或 mixed-case 缺陷升级为兼容承诺。

## Token、坐标与物理行

C1 token 独立保存 kind、语义 value（如适用）和权威 `full_lexeme_span=[start,end)`；span 不能从解码后的 value
长度反推。compiler 在 lexing 前验证 UTF-8，并只从每个 source 的开头剥离一个 `EF BB BF`。该 BOM 不形成 token，
随后 byte 0 是所有 C1/C2 span 的原点；ProgramHash 仍完全遵守 ADR-0017 的 exact source framing。设剥离后 source
长度为 `N`，C1 恰好产生一个 `EOF`，span 永远是 `[N,N)`。

LF、CR 和 CRLF 都产生一个 `Newline` token；span 分别覆盖 1、1、2 个 exact bytes，CRLF 不拆成两个 token。任何
raw-text、string 或 unknown-token 错误都不得吞掉物理 newline；错误 token 在 newline 起点前结束，随后仍产生独立
`Newline`。leading BOM 之外出现的 U+FEFF 不再被剥离，按普通输入接受后续 lexical classification。

## Statement-head raw-tail

`PRINT` 与 `ALERT` 只在 statement head 触发 raw-tail。statement head 是 BOF 或 `Newline` 后跳过 horizontal trivia
得到的第一个 lexeme；匹配只做 ASCII-insensitive 比较，不做 locale 或 Unicode case folding。C1 仍产生
`Identifier(original_spelling, full_lexeme_span)`，不得把拼写改成大写或小写；识别该 head 后进入 raw-tail，直到下一
物理 newline 或 EOF。

raw-tail 必须按以下顺序执行，C2 不得重新切 source：

1. 先把 head token 结束位置到下一 physical newline 起点之间的 exact bytes 固定为 `statement_tail`；若没有 newline，
   终点为 `N`。CRLF 的起点是 CR，两个 bytes 都不进入 tail。后续分 item 绝不越过这个边界。
2. 从 tail 起点或前一个 separator 后开始候选 item。跳过 ASCII space/tab 后，只有首个非 trivia byte 是 `'` 或 `"`
   才启用对应 quote 的 wrapper probe。probe 在同一 physical tail 中寻找一个同类 quote，使其后只剩 ASCII space/tab，
   再后就是 `&` 或 tail 终点；满足此条件的第一个同类 quote 是唯一合法 closer。closer 必须是 trim 后 item 的最后
   一个 byte；更早但不满足该位置条件的同类 quote 只是 payload。反斜杠没有 escape 含义，不能阻止满足位置条件的
   quote 成为 closer。
3. probe 找到合法 closer 时，opening 与 closer 之间的全部 `&` 都被 wrapper 包裹并属于 payload，item 在 closer 后
   trivia 之后的 `&` 或 tail 终点结束。probe 不存在或找不到合法 closer 时，第一个 `&` 就结束 item，即使前面已有
   opening quote。因此只有被合法 wrapper 包裹的 `&` 不形成 token；其他每个 `&` 都产生
   `Ampersand("&", full_lexeme_span)`。
4. 对 delimiter 之间的 raw slice 只 trim 首尾 ASCII space/tab，得到 `trim_span` 和 `trim_bytes`。trim 后为空时不产生
   content token、value 或 lexical diagnostic；相邻、开头或结尾的 `&` token 仍全部保留，语法缺项留给 C2。
5. 仅当 ordinary variable lexer 对全部 `trim_bytes` 恰好产生一个 `$name`、`$$name`、`_name` 或 `@name` token 且
   消耗到 trim 终点时，才产生该普通变量/常量/外部变量 token。prefix 只出现在开头、变量后还有 space/text，或
   ordinary variable lexer 未消费完整 item 时，都不算独立变量引用，整个 item 继续按 raw text 处理。
6. 非变量 item 若不以 quote 开始，产生 `RawText(trim_bytes, trim_span)`。若以 quote 开始且步骤 2 找到合法 closer，
   产生 `RawText(bytes_between_wrapper, trim_span)`；outer wrapper 不进 value，wrapper 内不再 trim。合法 closer 后只
   允许步骤 2 已排除在 `trim_span` 外的 ASCII space/tab，任何其他 byte 都使此前 quote 只成为 payload而不是 closer。
7. 以 quote 开始但没有合法 closer 的 item 仍产生一个 `RawText`：value 是 opening quote 后直到 `trim_span.end` 的
   exact bytes，包含所有未满足 closer 位置条件的同类 quote；同时产生
   `ECS_LEX_UNTERMINATED_RAW_TEXT`，diagnostic span 与 `trim_span` 相同。item delimiter、physical newline 和 EOF
   都不被该 token 或 diagnostic 吞并，lexer 从既定边界继续。

C1 对每个 raw item 产生：

```text
RawText(decoded_payload, full_lexeme_span)
```

`decoded_payload` 只表示已验证 UTF-8，并按上述算法移除合法 outer wrapper 或未闭合 item 的 opening quote；raw-tail
不执行任何 backslash escape 解码。`full_lexeme_span` 等于 `trim_span`，合法 wrapper 在 span 内但不进 value。
payload 的尾反斜杠必须原样保留给后续 E1 PRINT continuation 语义，C1/C2 都不得剥离。

以下反例矩阵是 exact token/diagnostic 合同；所有示例都无 BOM：

| Source | C1 在 head 后的唯一结果 |
| --- | --- |
| `PRINT $x\n` | `Var("$x",[6,8))`，`Newline[8,9)`，`EOF[9,9)` |
| `PRINT $x tail\n` | 整项不是变量；`RawText("$x tail",[6,13))`，`Newline[13,14)`，`EOF[14,14)` |
| `PRINT "a&b"\n` | `&` 被合法 wrapper 包裹；`RawText("a&b",[6,11))`，无 `Ampersand`，`Newline[11,12)` |
| `PRINT ""\n` | trim item 非空且 wrapper 合法；产生 `RawText("",[6,8))`，与不产生 content token 的 empty item 不同 |
| `PRINT "a"b\n` | quote `[8,9)` 不是 trim 后最后 byte，只是 payload；`RawText("a\"b",[6,10))` 加 `ECS_LEX_UNTERMINATED_RAW_TEXT@[6,10)`，再产生 `Newline[10,11)` 与 `EOF[11,11)` |
| `PRINT "a&b\n` | 没有合法 wrapper，`&[8,9)` 是 separator；`RawText("a",[6,8))` 加 `ECS_LEX_UNTERMINATED_RAW_TEXT@[6,8)`，随后 `Ampersand[8,9)`、`RawText("b",[9,10))`、`Newline[10,11)` |
| `PRINT A&& B &\n` | `RawText("A")`、三个 `Ampersand`、`RawText("B")`；两个 empty item 都不产生 content token 或 lexical diagnostic |

## Ordinary string

raw-tail 之外的 ordinary string 可以使用同类 `'...'` 或 `"..."` wrapper，只允许以下六种 escape：

| Source bytes | Decoded scalar |
| --- | --- |
| `\\` | backslash |
| `\"` | `"` |
| `\'` | `'` |
| `\n` | LF |
| `\r` | CR |
| `\t` | tab |

其他 escape、multiline string 和 escaped physical newline 一律禁止。合法 string 产生
`String(decoded_value, full_lexeme_span)`，span 覆盖 wrapper，value 不含 wrapper。

遇到无效 escape 时，C1 对整个 lexeme 只产生一个 `InvalidString(full_lexeme_span)`，不发布可被误用的 partial
decoded value；每个无效 escape 产生 `ECS_LEX_INVALID_ESCAPE`，diagnostic span 覆盖 backslash 与其后的 UTF-8
scalar。若其后是 physical newline 或 EOF，diagnostic 只覆盖 backslash，且不得消费边界。lexer 继续扫描到同类
closing quote、physical newline 或 EOF，以一次 token 保证确定性恢复。

ordinary string 在 closing quote 前遇到 physical newline 或 EOF 时，同一个 `InvalidString` 还产生
`ECS_LEX_UNTERMINATED_STRING`；即使此前已有 invalid escape，也不隐藏 unterminated 事实。token span 从 opening
wrapper 到 closing wrapper 之后，或在没有 closer 时到 newline/EOF 之前；unterminated diagnostic span 与该 token span
相同。escaped newline 因而同时保持 newline token 和稳定的 lexical diagnostics，不形成 multiline value。

## Case 分类与 downstream handoff

ordinary keywords、logic keywords 和 boolean literals 都按 ASCII-insensitive 识别。只允许 ASCII fold，不做当前
locale、Unicode normalization 或 Unicode case mapping。user symbol 的 UTF-8 bytes 始终 byte-wise case-sensitive；C1
保留 identifier 原始拼写，不把不同 user symbol 合并。

ADR-0017 的 builtin 分层保持不变：statement builtin 名 ASCII-insensitive，expression builtin 名 exact
case-sensitive。除了 PRINT/ALERT statement-head 为选择 raw-tail 所需的窄 probe，C1 不解析 builtin。C1 只产生
ordinary/logic keyword 与 boolean token，或保留 exact spelling 的 identifier/user-symbol token；statement/expression
syntactic context 与 builtin resolution 分别属于未来 C2/C3。冻结 case/handoff matrix 为：

| 输入 | C1 唯一证据 | 冻结的 downstream 结果与 owner |
| --- | --- | --- |
| `IF` / `If` / `if` | 都是 `IfKeyword` | C1 已闭合，无 builtin resolution |
| `AND` / `And` / `and` | 都是 `LogicAndKeyword` | C1 已闭合，无 builtin resolution |
| `TRUE` / `True` / `true` | 都是 `Boolean(true)` | C1 已闭合，无 builtin resolution |
| statement `LEN` / `Len` / `len` | 分别是保留原拼写的 `Identifier("LEN")` / `Identifier("Len")` / `Identifier("len")` | C2 证明 statement context，C3 按 ASCII-insensitive 解析为同一 statement builtin |
| expression `LEN(...)` | `Identifier("LEN")`，保留原拼写 | C2 证明 expression-call context，C3 exact-match 为 expression builtin |
| expression `Len(...)` / `len(...)` | 分别是 `Identifier("Len")` / `Identifier("len")` | C3 保持 case-sensitive user-call candidate，不解析为 expression builtin |
| `$Foo` / `$foo` | 两个保留原 bytes 的不同 user-symbol token | C1 已闭合；C2/C3 不得合并 identity |

未来 C2 必须用 `ecs.c2.builtin-context-case` 证明 statement/expression 的 syntactic context，未来 C3 必须用
`ecs.c3.builtin-resolution-case` 证明 ADR-0017 的 statement ASCII-insensitive 与 expression exact resolution。
这两个 assertion 都不是 C1 gate，`ecs.c1.lexer-case` 不得声称其通过。

## S0 42-byte exact token trace

五份 `corrected.print.*.input.ecs` 都是无 BOM、LF 分隔且末尾有 LF 的同一 42-byte source。C1 必须逐 byte 得到：

```text
[0,5)   Identifier("PRINT")   [6,10)  RawText("A\\")   [10,11) Newline
[11,16) Identifier("PRINT")   [17,21) RawText("B\\")   [21,22) Newline
[22,27) Identifier("PRINT")   [28,31) RawText("C")     [31,32) Newline
[32,37) Identifier("PRINT")   [38,41) RawText("D")     [41,42) Newline
[42,42) EOF
```

这里 notation 中 `RawText("A\\")` 的 value 是 `A` 加一个 byte `0x5C`；source span `[6,10)` 覆盖 opening quote、
`A`、尾反斜杠和 closing quote。反斜杠不转义 closing quote。该 trace 只证明 C1 token/value/span；C1 不得据此
声称四个 `PrintFragment`、PRINT continuation 或任何 evaluator outcome 已通过。

两份 `v1-native.heap-order.*.input.ecs` 在 C1/C2/C3 只能验证各自 exact source/hash 与 stage-relevant token/child-order
mapping。success 与 heap-limit failure 仍由 E1 执行 conformance 验证，不能提前为 C1 结论。

## Lexical diagnostics、恢复与排序

无法归类的合法 UTF-8 scalar 产生一个 `Unknown` token 和 `ECS_LEX_UNKNOWN_TOKEN`；token/diagnostic span 精确覆盖
该 scalar 的 UTF-8 bytes，lexer 至少推进一个 scalar。invalid UTF-8 属于 SourceBundle/loader preflight，发生在 lexing
前，不伪造成 `Unknown`。raw/string 恢复遵守前述单 token、newline 不吞并边界；所有路径最终仍产生唯一 EOF。

C1 lexical diagnostic 使用 `easycon-ecs::CompileDiagnostic`，并保持 ADR-0017 的全局稳定排序：main、按 raw UTF-8
`source_id` 排序后的 lib ordinal、byte start、phase rank、severity、stable code、emission ordinal。C1 lexer phase rank
早于 C2 parser。普通 diagnostic 的 `emission_ordinal` 是在同一 source/phase 中从 0 开始、按 scanner source order
分配的 would-be emission 序号；相同 sort fields 才由它打破平局。budget admission 按上述完整 canonical sort order
处理，不依赖 worker scheduling。

### Exact diagnostic saturation sentinel

每个普通 diagnostic admission 前同时检查当前 source 的 normal count 和 bundle normal total。若接纳后 source 将超过
64，或 bundle normal total 将超过 511，则该 would-be diagnostic 不进入结果；它成为唯一 trigger，并在 reserved slot
生成以下 exact `CompileDiagnostic`：

| 字段 | `ECS_DIAGNOSTIC_LIMIT` 冻结值 |
| --- | --- |
| `source_id` | trigger would-be diagnostic 的真实 source ID；不得使用 main/synthetic sentinel 替代 |
| `byte_span` | `[s,s)`，`s` 是 trigger would-be diagnostic 原 span 的 byte start；若 trigger 本来位于 EOF，则为 `[N,N)` |
| display line/column | 从 trigger source 的 zero-width `s` 按 ADR-0017 规则派生 |
| `phase` | trigger would-be diagnostic 的 phase；C1 lexer saturation 固定为 `Lexer` |
| `severity` | `Error` |
| stable code | `ECS_DIAGNOSTIC_LIMIT` |
| `emission_ordinal` | 被拒普通 diagnostic 的 would-be、0-based source/phase ordinal，不重新编号 |

sentinel 的语义 scope 是 bundle，但它仍使用 trigger source 作为 `CompileDiagnostic` 必填锚点。它以该真实 source
ordinal、zero-width start、`Lexer` rank、`Error`、stable code 和 would-be emission ordinal 参与 ADR-0017 普通排序；
不强制 append 到开头或末尾。这样 bundle-level 只表示它封闭整个 bundle 的 diagnostic budget，不表示另造一个
无法排序的 source。

第一个超限 trigger 原子设置 bundle `diagnostic_budget_sealed`。per-source 与 bundle-total 条件同时命中也只产生这一条
sentinel；此后当前及后续 compile phase 不再接纳普通 diagnostic，也不产生第二条 sentinel。已接纳 normal diagnostic
保持不变，因此每 source normal `<=64`、bundle normal `<=511`、加 reserved sentinel 后 total `<=512`。C1 仍按 token
合同保留唯一 `EOF[N,N)`；不得用 EOF span替换一个发生在更早 byte 的 saturation trigger。

exact saturation matrix 为：

| Case | 唯一结果 |
| --- | --- |
| main `main.ecs` 含 65 个单 byte unknown scalar | offsets 0..63 的 64 条 normal diagnostic 保留；第 65 个 scalar 起点 `s=64` 触发 `source_id=main.ecs`、`byte_span=[64,64)`、`phase=Lexer`、`emission_ordinal=64` 的 sentinel；source `N=65` 的 EOF 仍为 `[65,65)` |
| bundle 已有 511 条 normal，下一 Lexer candidate 来自 `lib/z.ecs@[9,10)`、would-be ordinal 7 | candidate 被抑制；sentinel 固定为 `source_id=lib/z.ecs`、`byte_span=[9,9)`、`phase=Lexer`、ordinal 7，并按 `lib/z.ecs` 的 source ordinal 排序 |
| trigger would-be diagnostic 原本是 zero-width EOF `[N,N)` | sentinel 也使用 `[N,N)`，不回退到前一 scalar |
| 第 65 条 source diagnostic 同时也是第 512 条 bundle normal candidate | 只产生一条 sentinel，normal counts 保持 source 64 / bundle 511，final total 512 |

## C1/C2 ownership

C1 拥有：

- SourceBundle、limits preflight、restricted loader、ProgramHash 和已验证 UTF-8/BOM view；
- raw-tail mode、ordinary string decode、keyword/boolean classification、token kind/value/span；
- lexical diagnostics、newline/EOF recovery、per-source emission ordinal 和交给全局 sorter 的 phase metadata。

C2 只能消费 C1 token stream，拥有 missing/unexpected token、错误 closer、unsupported trailing syntax、parser recovery、
AST 与 unexpected-EOF parser diagnostic；unexpected EOF 也只能引用既有 `[N,N)` EOF span。C2 不得重新切 source、重做
BOM/newline、重新解释 raw-tail 或 backslash、剥离 PRINT 尾反斜杠，或为同一 lexical defect 重复发出 C1 diagnostic。
它可以围绕 `InvalidString`/`Unknown` 同步语法，但不能把 lexical error 改写成 partial valid AST。

本合同不改变 binder 或 evaluator。statement/expression builtin 分层、boolean binding、PRINT continuation 和最终
OutputPort effect 仍分别由 ADR-0017 与其后拥有节点验证；C1/C2 的通过不能替代这些证据。

## C1 conformance 与 RED 顺序

C1 实现节点至少登记以下 stable assertion IDs；每个 ID 必须 exactly-one 映射到一个 passing、non-ignored exact test
和对应 production source marker，不能用一个组合测试冒充多条 mapping：

| Assertion ID | C1 证据 |
| --- | --- |
| `ecs.c1.lexer-raw-tail` | PRINT/ALERT head、physical-tail-first item 算法、trim/empty/exact-variable、wrapper/no-escape、separator、unterminated raw 与 42-byte trace |
| `ecs.c1.lexer-string-recovery` | 六种 valid escape、invalid escape、unterminated、newline/EOF recovery |
| `ecs.c1.lexer-case` | ordinary/logic keyword 与 boolean token 分类、identifier 原拼写及 user-symbol case preservation；不含 builtin context/resolution |
| `ecs.c1.diagnostic-span-sort` | BOM 后 byte span、LF/CRLF/CR、unknown、EOF、排序与 exact saturation sentinel |

C1 builder 必须按以下 RED 顺序推进；每一步先形成失败断言，再做满足该步的最小实现：

1. dependency guard whitelist；
2. SourceBundle/limits；
3. ProgramHash vectors；
4. restricted loader；
5. exact S0 raw-tail token trace；
6. ordinary valid/invalid/unterminated strings；
7. ordinary/logic keyword、boolean、identifier 原拼写与 user-symbol case-preservation matrix；
8. BOM + LF/CRLF/CR + unknown scalar + EOF + diagnostic ordering/saturation；
9. conformance mapping。

完成第 9 步、C1 全部门禁、固定 SHA 独立 implementation review 之前，不得启动 C2，也不得把 proposal review 当成
implementation review。未来 C2 language-contract 直接引用本 ADR 的 token/value/span 和 ownership，不再另建一套
source slicing 或 escape 规则。

## Proposal 与 acceptance 验证边界

本合同的 proposal 与 acceptance 只新增或更新本 ADR，并同步根 README 与 docs README 的索引/当前状态。两次提交前
都只运行 docs-only 门禁：

```powershell
python -B tools/check_markdown_links.py
python -B tools/check_repository_guards.py
git diff --check
```

这些结果只证明 Markdown 引用、repository boundary 与 diff hygiene，不证明 Rust 编译、lexer behavior、fixture
reproduction、conformance、C1/C2、Workspace、CI、硬件或发布已经通过。

## 关联

- [ADR-0017：冻结 Phase 4 ECS 与 Automation 目标](0017-phase-4-ecs-automation-target.md)
- [ADR-0018：窄重开 Phase 2A Controller lease 结算合同](0018-phase-2a-controller-lease-reopen.md)
- [架构总览](../architecture/architecture-overview.md)
- [源码能力映射](../architecture/source-capability-map.md)
- [测试策略](../architecture/testing-strategy.md)
- [实施路线](../architecture/repository-roadmap.md)
