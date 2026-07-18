# Traceable Markdown Document Research

本上下文描述对受控、版本化 Markdown 文档语料进行的可溯源研究。Canonical Markdown 文档正文是唯一事实真源；模型自身知识可以补充答案，但不能成为 Verbatim Source Evidence。

## 研究生命周期

**Document Research Conversation**:
由多个相关 Document Research Request 构成的长期研究上下文。已完成答案可以帮助理解后续问题，但不能成为后续问题的 Verbatim Source Evidence。
_Avoid_: Research Conversation、session、chat history

**Document Research Request**:
Document Research Conversation 内一个用户问题从 Research Question Clarification 开始，到完成、失败或取消结束的工作单元。
_Avoid_: Research Turn、message、Research Run

**Research Question Clarification**:
消除用户问题中会改变研究范围的歧义，并形成 Document Research Brief 的自然语言过程。
_Avoid_: Clarification、intake form、questionnaire

**Document Research Brief**:
对用户原问题、已知研究上下文、研究假设、未决歧义和回答要求的规范化表达。
_Avoid_: Research Brief、prompt、query

**Frozen Document Research Brief**:
作为 Markdown Research Execution 语义输入且不可再修改的 Document Research Brief。
_Avoid_: Frozen Research Brief、user-confirmed form

**Markdown Research Execution**:
针对一个 Frozen Document Research Brief 和一个 Markdown Corpus Snapshot 执行的一次有界、固定研究流程。
_Avoid_: Research Run、Document Research Conversation、Document Research Request

**Markdown Research Execution Limits**:
一个 Markdown Research Execution 冻结的资源、并发和停止限制集合。
_Avoid_: Research Policy、policy、workflow、Answer Style

## Markdown 文档语料

**Markdown Corpus Snapshot**:
供一个 Markdown Research Execution 使用的 Markdown 文档、文档版本、正文片段边界和导航关系的不可变发布视图。
_Avoid_: Knowledge Snapshot、database snapshot、search index

**Markdown Corpus Navigation Node**:
用于渐进披露更窄 Markdown 语料方向的导航分组。它不是 Verbatim Source Evidence，也不是本体断言。
_Avoid_: Topic Node、category fact、knowledge graph node

**Markdown Source Document**:
一篇 Markdown 事实来源跨修订保持稳定的身份。
_Avoid_: Document、file path、Content Unit

**Markdown Source Document Version**:
Markdown Source Document 的一份不可变正文版本，可被一个或多个 Markdown Corpus Snapshot 引用。
_Avoid_: Document Version、Knowledge Snapshot

**Canonical Markdown Document Body**:
经过版本化 canonicalization、可以产生 Verbatim Source Evidence 的 Markdown 源文本。
_Avoid_: Document Body、rendered page、summary

**Markdown Source Segment**:
Canonical Markdown Document Body 中为受控读取和精确定位而机械划定的区域。
_Avoid_: Content Unit、topic、semantic chunk

## 研究判断

**Research Document Read Request**:
说明一次 Markdown Source Document 或 Markdown Source Segment 读取预计解决哪个未决研究问题的执行内请求。
_Avoid_: Read Intent、search query、tool call

**Markdown Source Review Decision**:
强模型读取一个 Markdown Source Segment 后，对提取 Verbatim Source Evidence、继续读取、扩大导航范围或关闭导航分支提出的判断。
_Avoid_: Read Outcome、Evidence、final answer

**Research Coverage Gap**:
会影响 Markdown Research Execution 停止条件，必须解决、说明无法解决或公开披露的未决研究问题。
_Avoid_: Research Gap、error、missing document

## Verbatim Source Evidence 与答案

**Verbatim Source Evidence**:
Canonical Markdown Document Body 中带稳定来源定位、经程序验证的逐字片段。
_Avoid_: Evidence、summary、model rationale

**Evidence-Linked Research Claim**:
模型在一个 Markdown Research Execution 内根据 Verbatim Source Evidence 形成的解释。它不是可跨执行复用的文档知识。
_Avoid_: Claim、fact、Evidence

**Research Claim Evidence Relationship**:
在当前 Markdown Research Execution 内声明一条 Verbatim Source Evidence 支持、限定或反驳一个 Evidence-Linked Research Claim 的关系。
_Avoid_: Claim Evidence Link、proof

**Model-Knowledge-Only Answer**:
不访问当前 Markdown 文档语料、只根据模型自身知识生成并作为未验证合成输入保留的答案。
_Avoid_: Model Knowledge Answer、Evidence answer、baseline truth

**Evidence-Linked Research Claims Answer**:
只根据当前 Markdown Research Execution 的 Evidence-Linked Research Claim 生成的答案。
_Avoid_: Claim Answer、verified truth、Model Knowledge Answer

**Answer Composition Style**:
决定 Model-Knowledge-Only Answer 与 Evidence-Linked Research Claims Answer 以何者为合成基底的模式。一次执行可以请求一种或两种 Answer Composition Style。
_Avoid_: Answer Style、Research Policy、精确数值权重

**Source-Attributed Answer Composition**:
按一种 Answer Composition Style 合成 Model-Knowledge-Only Answer 与 Evidence-Linked Research Claims Answer，并为每个输出段保留来源类型的结果。
_Avoid_: Source-Aware Answer Composition、Answer Composition、Claim Answer

**Source-Attributed Answer Segment**:
Source-Attributed Answer Composition 中一段带明确来源类型、相关 Evidence-Linked Research Claim 和 Public Source Citation 的最终回答文本。
_Avoid_: Answer Segment、text、mixed content

**Public Source Citation**:
Verbatim Source Evidence 的来源元数据和逐字内容经过公开策略筛选后的投影。
_Avoid_: Public Citation、internal source reference

**Markdown Research Execution Trace**:
一次 Markdown Research Execution 的候选、判断、读取、校验结果、状态转换和终态构成的 append-only 可观察记录。
_Avoid_: Research Trace、debug log、hidden chain-of-thought
