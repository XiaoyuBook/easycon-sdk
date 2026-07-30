# 0019：提议冻结 Phase 4 C1 lexer 合同

- 状态：Proposed / Not Effective
- 提议日期：2026-07-31
- 固定起点：`ecbd39f4fc89a4a0c3768b0689e7ae495c29d664`
- 上位目标：[ADR-0017](0017-phase-4-ecs-automation-target.md) 的
  `Accepted / Frozen Phase 4 ECS/Automation Target`
- S0 证据：[ECS provenance manifest](../../spec/fixtures/ecs/manifest.json) 与
  [fixture generator](../../tools/generate_ecs_provenance_fixtures.py)
- 只读裁定：任务 `019fb3c0-17dd-7eb0-a00a-ee09e881049d`
- 编号说明：ADR-0018 已被 Controller D0 占用，本提议使用 0019

## 状态、生效条件与范围

本文件只是 C1 lexer contract proposal，不是已接受决定，不冻结实现，也不授权或表示 C1 已经启动。它必须依次完成：

1. 固定本 proposal 的 Git SHA/tree/parent，由新的独立 reviewer 对固定对象做只读完整审查；
2. 清零全部可复现、可行动且 in-scope 的 P0/P1/P2 finding；
3. 由后续单独的 docs-only acceptance/freeze commit 把本 ADR 推进为 Accepted/Frozen。

第三步完成前，C1 保持未启动；本 proposal 的 docs-only 门禁不能充当 C1 实现、测试或 conformance 证据。任何
语义修订都必须形成新的固定候选并重新接受独立审查。候选和未来 acceptance 的自身 SHA/tree 只由提交后的 Git
对象、固定 ref 与外部结构化报告记录，不在 tracked 文件中预言。

本提议只补齐 ADR-0017 未唯一回答的 lexer 决策，不修改其已经冻结的 ownership、ProgramHash、单个 leading BOM
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

raw-tail 是由 `&` 分隔的 item 序列；`&` 无条件产生 `Ampersand("&", full_lexeme_span)`，即使此前已出现 opening
quote 也仍是 separator，wrapper 不能引用 `&`。物理 newline 无条件结束 raw-tail。一个在 item
边界独立出现的 `$name`、`$$name`、`_name` 或 `@name` 使用普通变量/常量/外部变量 token，而不并入相邻文本；嵌在
未分隔 raw text 中的同样 bytes 只是 payload。head、`&` 或独立变量引用周围的 ASCII space/tab 是 trivia。未包装
raw item 的首尾 trivia 不进入 value，内部空白保留。

raw item 可以不用 wrapper，也可以在同一个 separator-delimited item 内用一对同类 `'` 或 `"` wrapper。反斜杠在
wrapper 内没有 escape 含义，因此 item 末尾的同类 quote 即使紧跟反斜杠也会关闭 wrapper；不同类 quote 只是
payload。以 quote 开始的 item 必须在 `&`、physical newline 或 EOF 前由同类 quote 关闭。

C1 对每个 raw item 产生：

```text
RawText(decoded_payload, full_lexeme_span)
```

`decoded_payload` 只表示已验证 UTF-8 并移除可选 outer wrapper；raw-tail 不执行任何 backslash escape 解码。
wrapper 不进入 value，但 `full_lexeme_span` 覆盖完整 wrapper。payload 的尾反斜杠必须原样保留给后续 E1 PRINT
continuation 语义，C1/C2 都不得剥离。若同类 closing wrapper 在 physical newline 或 EOF 前不存在，C1 仍产生覆盖
opening wrapper 至该边界前全部 bytes 的 `RawText`，value 排除 opening wrapper，并产生
`ECS_LEX_UNTERMINATED_RAW_TEXT`；diagnostic span 与该 token 的 `full_lexeme_span` 相同，separator/newline/EOF
保持上述独立边界。

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

## Case 分类

ordinary keywords、logic keywords 和 boolean literals 都按 ASCII-insensitive 识别。只允许 ASCII fold，不做当前
locale、Unicode normalization 或 Unicode case mapping。user symbol 的 UTF-8 bytes 始终 byte-wise case-sensitive；C1
保留 identifier 原始拼写，不把不同 user symbol 合并。

ADR-0017 的 builtin 分层保持不变：statement builtin 名 ASCII-insensitive，expression builtin 名 exact
case-sensitive。除了 PRINT/ALERT statement-head 为选择 raw-tail 所需的窄 probe，C1 不把 builtin 解析结果编码成
不同的 user-symbol identity；C2/C3 按 syntactic position 消费保留原拼写的 identifier。冻结 case matrix 为：

| 输入 | 冻结结果 |
| --- | --- |
| `IF` / `If` / `if` | 都是 `IfKeyword` |
| `TRUE` / `True` / `true` | 都是 `Boolean(true)` |
| statement `LEN` / `Len` / `len` | 都按 statement builtin 处理 |
| expression `LEN(...)` | expression builtin |
| expression `Len(...)` / `len(...)` | case-sensitive user-call candidate |
| `$Foo` / `$foo` | 两个不同 user symbol |

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
早于 C2 parser；同一 code 的多次 emission 才由 emission ordinal 保序。每 source 64、normal total 511、reserved
`ECS_DIAGNOSTIC_LIMIT` 后 total 不超过 512 的 saturation 合同原样保留；到达上限后停止该 phase 的额外诊断，不用
不稳定尾部错误替换预留 limit diagnostic。

## C1/C2 ownership

C1 拥有：

- SourceBundle、limits preflight、restricted loader、ProgramHash 和已验证 UTF-8/BOM view；
- raw-tail mode、ordinary string decode、keyword/boolean classification、token kind/value/span；
- lexical diagnostics、newline/EOF recovery、per-source emission ordinal 和交给全局 sorter 的 phase metadata。

C2 只能消费 C1 token stream，拥有 missing/unexpected token、错误 closer、unsupported trailing syntax、parser recovery、
AST 与 unexpected-EOF parser diagnostic；unexpected EOF 也只能引用既有 `[N,N)` EOF span。C2 不得重新切 source、重做
BOM/newline、重新解释 raw-tail 或 backslash、剥离 PRINT 尾反斜杠，或为同一 lexical defect 重复发出 C1 diagnostic。
它可以围绕 `InvalidString`/`Unknown` 同步语法，但不能把 lexical error 改写成 partial valid AST。

本提议不改变 binder 或 evaluator。statement/expression builtin 分层、boolean binding、PRINT continuation 和最终
OutputPort effect 仍分别由 ADR-0017 与其后拥有节点验证；C1/C2 的通过不能替代这些证据。

## C1 conformance 与 RED 顺序

C1 实现节点至少登记以下 stable assertion IDs；每个 ID 必须 exactly-one 映射到一个 passing、non-ignored exact test
和对应 production source marker，不能用一个组合测试冒充多条 mapping：

| Assertion ID | C1 证据 |
| --- | --- |
| `ecs.c1.lexer-raw-tail` | PRINT/ALERT head、wrapper/no-escape、separator、unterminated raw 与 42-byte trace |
| `ecs.c1.lexer-string-recovery` | 六种 valid escape、invalid escape、unterminated、newline/EOF recovery |
| `ecs.c1.lexer-case` | 上述 keyword/boolean/builtin/user-symbol case matrix |
| `ecs.c1.diagnostic-span-sort` | BOM 后 byte span、LF/CRLF/CR、unknown、EOF、排序与 saturation |

C1 builder 必须按以下 RED 顺序推进；每一步先形成失败断言，再做满足该步的最小实现：

1. dependency guard whitelist；
2. SourceBundle/limits；
3. ProgramHash vectors；
4. restricted loader；
5. exact S0 raw-tail token trace；
6. ordinary valid/invalid/unterminated strings；
7. keyword/boolean/builtin case matrix；
8. BOM + LF/CRLF/CR + unknown scalar + EOF + diagnostic ordering/saturation；
9. conformance mapping。

完成第 9 步、C1 全部门禁、固定 SHA 独立 implementation review 之前，不得启动 C2，也不得把 proposal review 当成
implementation review。未来 C2 language-contract 直接引用本 ADR 的 token/value/span 和 ownership，不再另建一套
source slicing 或 escape 规则。

## Proposal 验证边界

本 proposal 只新增本 ADR，并更新根 README 与 docs README 的索引/当前状态。提交前只运行 docs-only 门禁：

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
