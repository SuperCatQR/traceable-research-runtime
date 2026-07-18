import type {
  CorpusSnapshot,
  DemoFixtures,
  ResearchConversation,
  ResearchRequest,
} from "./types";

const currentLaborSnapshot: CorpusSnapshot = {
  id: "snapshot-cn-labor-2026-07-15",
  name: "劳动用工法规与裁判规则库",
  versionLabel: "2026-07-15 冻结版",
  publishedAt: "2026-07-15T09:00:00+08:00",
  documentCount: 42,
  contentHash:
    "sha256:89b5fa7cc136bde53ed639e5c52caa73a179ab70eb6c37cc0da52183e72ba812",
  availability: "available",
};

const previousLaborSnapshot: CorpusSnapshot = {
  id: "snapshot-cn-labor-2025-01-01",
  name: "劳动用工法规与裁判规则库",
  versionLabel: "2025-01-01 历史冻结版",
  publishedAt: "2025-01-01T09:00:00+08:00",
  documentCount: 38,
  contentHash:
    "sha256:45e0aefb11e103852e70e70fd5ca0f37f184dbf23f65369fe987f037dd1bf6e4",
  availability: "available",
};

const archivedCaseSnapshot: CorpusSnapshot = {
  id: "snapshot-cn-employment-cases-2024-06-30",
  name: "劳动争议案例专题库",
  versionLabel: "2024-06-30 归档版",
  publishedAt: "2024-06-30T18:00:00+08:00",
  documentCount: 126,
  contentHash:
    "sha256:0aab6c9677807df9ad82e5bdde1ddb83985ad695360a8ecbf1b103e82b33fa46",
  availability: "unavailable",
};

export const normalRequest: ResearchRequest = {
  id: "request-economic-compensation-001",
  number: 1,
  shortTitle: "协商解除的经济补偿核算",
  originalQuestion:
    "公司提出协商解除劳动合同。我在公司工作了 8 年 4 个月，解除前 12 个月平均应发工资是 24,000 元，经济补偿应该怎么算？",
  clarifiedQuestion:
    "在用人单位提出并与劳动者协商一致解除劳动合同的前提下，依据冻结语料说明经济补偿的适用条件、工作年限折算和月工资基数；暂按 8 年 4 个月及解除前 12 个月平均应发工资 24,000 元测算，并披露因工作地、解除日期及当地上年度职工月平均工资缺失而无法核定三倍封顶标准的限制。",
  status: "completed",
  statusLabel: "有限结果 · 已完成",
  snapshot: currentLaborSnapshot,
  requestedModes: ["evidence-first", "model-led"],
  phases: [
    {
      id: "phase-clarify",
      label: "界定问题",
      detail: "确认由用人单位提出协商解除，并识别地域工资数据缺口",
      status: "complete",
    },
    {
      id: "phase-snapshot",
      label: "冻结语料",
      detail: "锁定 2026-07-15 版法规与裁判规则库",
      status: "complete",
    },
    {
      id: "phase-navigate",
      label: "定位规则",
      detail: "沿解除事由、年限折算和工资口径 3 条路径检索",
      status: "complete",
    },
    {
      id: "phase-read",
      label: "读取原文",
      detail: "选取 4 份文档并完成 9 次可追踪片段读取",
      status: "complete",
    },
    {
      id: "phase-validate",
      label: "核验证据",
      detail: "接受 4 条逐字证据，披露 1 个高优先级覆盖缺口",
      status: "complete",
    },
    {
      id: "phase-synthesize",
      label: "生成交付",
      detail: "输出证据优先与模型解读两种有限回答",
      status: "complete",
    },
  ],
  counts: {
    navigationBranches: 3,
    selectedDocuments: 4,
    segmentReads: 9,
    acceptedEvidence: 4,
  },
  selectedNavigationLabels: [
    "解除或终止事由 / 协商解除",
    "经济补偿 / 工作年限折算",
    "经济补偿 / 月工资口径与三倍封顶",
  ],
  selectedDocumentTitles: [
    "中华人民共和国劳动合同法",
    "中华人民共和国劳动合同法实施条例",
    "深圳市劳动争议裁审衔接工作指引（现行收录版）",
    "城镇单位就业人员平均工资发布说明（索引页）",
  ],
  stopReason:
    "核心法条已经形成闭环，但工作地、解除生效日期及对应年度当地职工月平均工资未提供，无法判断 24,000 元是否触发三倍封顶。系统依照有限回答策略停止扩展并明确披露缺口。",
  answers: [
    {
      mode: "evidence-first",
      title: "证据优先回答",
      summary:
        "在“由公司提出并协商一致解除”的假设成立时，原则上应支付经济补偿。8 年 4 个月按 8.5 个月工资折算；若暂不触发法定三倍封顶，以 24,000 元为基数的暂算额为 204,000 元。地域及解除日期缺失，因此这不是最终核定金额。",
      blocks: [
        {
          id: "answer-evidence-trigger",
          label: "支付前提",
          kind: "evidence",
          text:
            "协商解除并不当然产生经济补偿。现有问题明确由公司提出；若双方最终依《劳动合同法》第三十六条协商一致解除，则落入第四十六条第二项规定的支付情形。应保留公司提出解除以及双方达成一致的书面材料。",
          citationIds: ["citation-article-46"],
        },
        {
          id: "answer-evidence-tenure",
          label: "年限折算",
          kind: "evidence",
          text:
            "每满一年计一个月；剩余 4 个月不满 6 个月，计半个月。因此 8 年 4 个月折算为 8.5 个月工资。该折算只处理年限，不代表工资基数已完成核定。",
          citationIds: ["citation-article-47-tenure"],
        },
        {
          id: "answer-evidence-base",
          label: "工资基数",
          kind: "hybrid",
          text:
            "月工资通常取解除前 12 个月平均应得工资，并包括奖金、津贴和补贴等货币性收入。按用户提供的 24,000 元暂算：8.5 × 24,000 = 204,000 元。若 24,000 元高于适用地区上年度职工月平均工资三倍，则月基数须改按三倍数额计算；本次因地域与年度数据缺失，不能确定是否封顶。",
          citationIds: [
            "citation-article-47-cap",
            "citation-regulation-27",
          ],
          modelNotice:
            "算式由模型依据已核验法条与用户输入生成；204,000 元仅为未触发三倍封顶时的条件性测算。",
        },
        {
          id: "answer-evidence-boundary",
          label: "结论边界",
          kind: "model",
          text:
            "请补充劳动合同履行地或用人单位所在地、解除生效日期、当地统计口径，以及 24,000 元是否为包含奖金津贴的应得工资，再进行最终核算。本回答是离线 Demo fixture 展示的研究结果，不构成法律意见或个案法律建议。",
          citationIds: [],
          modelNotice: "本段是风险提示，不是语料原文。",
        },
      ],
    },
    {
      mode: "model-led",
      title: "模型解读回答",
      summary:
        "当前最清晰的工作结论是“满足公司提出协商解除时应补偿，暂按 204,000 元测算，但须先核验当地三倍社平工资上限”。模型将事实代入规则，逐字依据仍可单独回看。",
      blocks: [
        {
          id: "answer-model-conclusion",
          label: "先说结论",
          kind: "hybrid",
          text:
            "如果确由公司提出并最终协商一致解除，支付义务具有明确法条依据。8 年 4 个月折算 8.5 个月；以 24,000 元作为未封顶基数时，条件性结果为 204,000 元。",
          citationIds: [
            "citation-article-46",
            "citation-article-47-tenure",
          ],
          modelNotice: "结论包含模型对用户事实的条件性归纳。",
        },
        {
          id: "answer-model-checklist",
          label: "核算清单",
          kind: "model",
          text:
            "第一步确认是谁提出解除及协议表述；第二步核对连续工作年限；第三步按解除前 12 个月应得工资复算平均值；第四步根据解除日期匹配当地统计年度；第五步比较 24,000 元与当地职工月平均工资三倍。任一输入变化都可能改变最终金额。",
          citationIds: [],
          modelNotice:
            "清单由模型组织，用于解释核算流程，不替代劳动仲裁机构或法院的认定。",
        },
        {
          id: "answer-model-authority",
          label: "可核验依据",
          kind: "evidence",
          text:
            "年限折算、三倍封顶和应得工资范围均有逐字证据支撑。界面中的引用可回到冻结快照、具体条款和原始追踪序号进行复核。",
          citationIds: [
            "citation-article-47-tenure",
            "citation-article-47-cap",
            "citation-regulation-27",
          ],
        },
        {
          id: "answer-model-disclaimer",
          label: "使用限制",
          kind: "model",
          text:
            "这是产品演示所用的固定研究样例，不构成法律建议。签署解除协议或主张具体金额前，应由具备资质的专业人士结合完整材料复核。",
          citationIds: [],
          modelNotice: "固定免责声明。",
        },
      ],
    },
  ],
  citations: [
    {
      id: "citation-article-46",
      claimId: "claim-payment-trigger",
      documentId: "cn-labor-contract-law",
      documentTitle: "中华人民共和国劳动合同法",
      sectionHeading: "第四十六条 经济补偿",
      quote:
        "有下列情形之一的，用人单位应当向劳动者支付经济补偿：（二）用人单位依照本法第三十六条规定向劳动者提出解除劳动合同并与劳动者协商一致解除劳动合同的；",
      versionHash:
        "sha256:20dbd95d91a9ee9bf6aa17773e812df7a81768ca28c3c481d425addbb9157612",
      traceSequence: 9,
    },
    {
      id: "citation-article-47-tenure",
      claimId: "claim-tenure-rounding",
      documentId: "cn-labor-contract-law",
      documentTitle: "中华人民共和国劳动合同法",
      sectionHeading: "第四十七条 第一款",
      quote:
        "经济补偿按劳动者在本单位工作的年限，每满一年支付一个月工资的标准向劳动者支付。六个月以上不满一年的，按一年计算；不满六个月的，向劳动者支付半个月工资的经济补偿。",
      versionHash:
        "sha256:20dbd95d91a9ee9bf6aa17773e812df7a81768ca28c3c481d425addbb9157612",
      traceSequence: 11,
    },
    {
      id: "citation-article-47-cap",
      claimId: "claim-salary-cap",
      documentId: "cn-labor-contract-law",
      documentTitle: "中华人民共和国劳动合同法",
      sectionHeading: "第四十七条 第二款",
      quote:
        "劳动者月工资高于用人单位所在直辖市、设区的市级人民政府公布的本地区上年度职工月平均工资三倍的，向其支付经济补偿的标准按职工月平均工资三倍的数额支付，向其支付经济补偿的年限最高不超过十二年。",
      versionHash:
        "sha256:20dbd95d91a9ee9bf6aa17773e812df7a81768ca28c3c481d425addbb9157612",
      traceSequence: 13,
    },
    {
      id: "citation-regulation-27",
      claimId: "claim-earned-wage-components",
      documentId: "cn-labor-contract-law-regulation",
      documentTitle: "中华人民共和国劳动合同法实施条例",
      sectionHeading: "第二十七条",
      quote:
        "劳动合同法第四十七条规定的经济补偿的月工资按照劳动者应得工资计算，包括计时工资或者计件工资以及奖金、津贴和补贴等货币性收入。",
      versionHash:
        "sha256:33ffb24299e70f2a534d5bddf49863a120e1ee255fddbfdcdd06198248e95a8e",
      traceSequence: 16,
    },
  ],
  coverageGaps: [
    {
      id: "gap-local-salary-cap",
      priority: "high",
      status: "disclosed",
      question: "24,000 元是否超过适用地区上年度职工月平均工资的三倍？",
      explanation:
        "用户未提供工作地、用人单位所在地和解除生效日期，无法选择适用地区及统计年度，也就不能核定三倍封顶基数。该缺口直接影响最终补偿金额，因此仅交付带条件的有限结论。",
    },
  ],
  audit: [
    {
      sequence: 1,
      eventType: "ResearchRequestCreated",
      summary: "创建研究请求 #1，并保存用户原始问题",
      time: "14:02:11",
    },
    {
      sequence: 2,
      eventType: "QuestionClarified",
      summary: "确认公司提出协商解除；登记地域与解除日期缺口",
      time: "14:02:28",
    },
    {
      sequence: 3,
      eventType: "ExecutionStarted",
      summary: "以有限回答策略启动固定研究引擎",
      time: "14:02:30",
    },
    {
      sequence: 4,
      eventType: "SnapshotPinned",
      summary: "冻结 snapshot-cn-labor-2026-07-15，共 42 份文档",
      time: "14:02:30",
    },
    {
      sequence: 5,
      eventType: "NavigationRequested",
      summary: "发起解除事由、年限折算与工资基数三条导航分支",
      time: "14:02:31",
    },
    {
      sequence: 6,
      eventType: "NavigationResultsAccepted",
      summary: "接受 3 条规范化导航路径",
      time: "14:02:32",
    },
    {
      sequence: 7,
      eventType: "DocumentSelected",
      summary: "选择《中华人民共和国劳动合同法》",
      time: "14:02:33",
    },
    {
      sequence: 8,
      eventType: "SegmentReadRequested",
      summary: "请求读取第四十六条协商解除相关片段",
      time: "14:02:34",
    },
    {
      sequence: 9,
      eventType: "SegmentReadCompleted",
      summary: "返回第四十六条逐字片段并记录内容哈希",
      time: "14:02:35",
    },
    {
      sequence: 10,
      eventType: "SegmentReadRequested",
      summary: "请求读取第四十七条年限折算片段",
      time: "14:02:36",
    },
    {
      sequence: 11,
      eventType: "SegmentReadCompleted",
      summary: "返回第四十七条第一款逐字片段",
      time: "14:02:37",
    },
    {
      sequence: 12,
      eventType: "SegmentReadRequested",
      summary: "请求读取第四十七条三倍封顶片段",
      time: "14:02:38",
    },
    {
      sequence: 13,
      eventType: "SegmentReadCompleted",
      summary: "返回第四十七条第二款逐字片段",
      time: "14:02:39",
    },
    {
      sequence: 14,
      eventType: "DocumentSelected",
      summary: "选择《中华人民共和国劳动合同法实施条例》",
      time: "14:02:40",
    },
    {
      sequence: 15,
      eventType: "SegmentReadRequested",
      summary: "请求读取第二十七条应得工资口径",
      time: "14:02:41",
    },
    {
      sequence: 16,
      eventType: "SegmentReadCompleted",
      summary: "返回第二十七条逐字片段；累计完成 9 次片段读取",
      time: "14:02:42",
    },
    {
      sequence: 17,
      eventType: "EvidenceValidated",
      summary: "4 条候选证据通过快照、坐标和逐字内容校验",
      time: "14:02:44",
    },
    {
      sequence: 18,
      eventType: "CoverageGapDisclosed",
      summary: "披露当地职工月平均工资三倍封顶的高优先级缺口",
      time: "14:02:45",
    },
    {
      sequence: 19,
      eventType: "AnswerSynthesized",
      summary: "生成证据优先回答，并校验全部引用 ID",
      time: "14:02:47",
    },
    {
      sequence: 20,
      eventType: "AnswerSynthesized",
      summary: "生成模型解读回答，并标记模型生成段落",
      time: "14:02:49",
    },
    {
      sequence: 21,
      eventType: "ExecutionCompleted",
      summary: "以有限结果完成；检查点、答案与审计投影已持久化",
      time: "14:02:50",
    },
  ],
  updatedAt: "2026-07-18T14:02:50+08:00",
};

export const recoveryRequest: ResearchRequest = {
  id: "request-economic-compensation-002",
  number: 2,
  shortTitle: "补全深圳封顶标准后重新核算",
  originalQuestion:
    "补充信息：工作地是深圳，计划 2026 年 7 月 31 日解除。请沿用上一次的 8 年 4 个月和月均应发 24,000 元，核实封顶后重新计算。",
  clarifiedQuestion:
    "基于同一劳动合同经济补偿问题，核实深圳在 2026-07-31 解除时适用的上年度职工月平均工资统计口径，判断 24,000 元是否触发三倍封顶，并在已确认的 8.5 个月补偿年限基础上复算。",
  status: "interrupted",
  statusLabel: "可重试中断 · 检查点已保留",
  snapshot: currentLaborSnapshot,
  requestedModes: ["evidence-first", "model-led"],
  phases: [
    {
      id: "recovery-phase-clarify",
      label: "界定问题",
      detail: "已补齐深圳工作地与 2026-07-31 解除日期",
      status: "complete",
    },
    {
      id: "recovery-phase-snapshot",
      label: "冻结语料",
      detail: "复用并校验 2026-07-15 冻结快照",
      status: "complete",
    },
    {
      id: "recovery-phase-navigate",
      label: "定位规则",
      detail: "已完成全国规则与深圳统计口径两条导航分支",
      status: "complete",
    },
    {
      id: "recovery-phase-read",
      label: "读取原文",
      detail: "第 5 次片段抽取遇到可重试的模型服务 503；恢复后从此处继续",
      status: "attention",
    },
    {
      id: "recovery-phase-validate",
      label: "核验证据",
      detail: "等待补齐深圳统计发布页证据后执行完整校验",
      status: "pending",
    },
    {
      id: "recovery-phase-synthesize",
      label: "生成交付",
      detail: "证据未闭环，尚未生成回答",
      status: "pending",
    },
  ],
  counts: {
    navigationBranches: 2,
    selectedDocuments: 3,
    segmentReads: 4,
    acceptedEvidence: 2,
  },
  selectedNavigationLabels: [
    "经济补偿 / 月工资口径与三倍封顶",
    "深圳 / 统计公报 / 职工月平均工资",
  ],
  selectedDocumentTitles: [
    "中华人民共和国劳动合同法",
    "中华人民共和国劳动合同法实施条例",
    "深圳市城镇单位就业人员年平均工资数据发布说明",
  ],
  stopReason:
    "模型适配器在第 5 次片段的候选抽取请求中收到可重试 HTTP 503。执行未写入失败终态；已持久化 2 条导航分支、3 份已选文档、4 次已完成读取和 2 条已接受证据。点击重试后将从追踪序号 11 继续，不重复既有读取。此 Demo fixture 不构成法律建议。",
  answers: [],
  citations: [
    {
      id: "recovery-citation-article-47-cap",
      claimId: "recovery-claim-salary-cap",
      documentId: "cn-labor-contract-law",
      documentTitle: "中华人民共和国劳动合同法",
      sectionHeading: "第四十七条 第二款",
      quote:
        "劳动者月工资高于用人单位所在直辖市、设区的市级人民政府公布的本地区上年度职工月平均工资三倍的，向其支付经济补偿的标准按职工月平均工资三倍的数额支付，向其支付经济补偿的年限最高不超过十二年。",
      versionHash:
        "sha256:20dbd95d91a9ee9bf6aa17773e812df7a81768ca28c3c481d425addbb9157612",
      traceSequence: 8,
    },
    {
      id: "recovery-citation-regulation-27",
      claimId: "recovery-claim-earned-wage-components",
      documentId: "cn-labor-contract-law-regulation",
      documentTitle: "中华人民共和国劳动合同法实施条例",
      sectionHeading: "第二十七条",
      quote:
        "劳动合同法第四十七条规定的经济补偿的月工资按照劳动者应得工资计算，包括计时工资或者计件工资以及奖金、津贴和补贴等货币性收入。",
      versionHash:
        "sha256:33ffb24299e70f2a534d5bddf49863a120e1ee255fddbfdcdd06198248e95a8e",
      traceSequence: 10,
    },
  ],
  coverageGaps: [
    {
      id: "recovery-gap-shenzhen-statistic",
      priority: "high",
      status: "unresolved",
      question: "2026-07-31 解除时应采用深圳哪一年度、哪一统计口径的月平均工资？",
      explanation:
        "目标统计发布页已经选中，但对应片段尚未完成抽取和逐字校验。恢复执行前不能据此计算封顶基数。",
    },
  ],
  audit: [
    {
      sequence: 1,
      eventType: "ResearchRequestCreated",
      summary: "创建后续研究请求 #2，关联同一经济补偿会话",
      time: "14:18:03",
    },
    {
      sequence: 2,
      eventType: "QuestionClarified",
      summary: "登记深圳工作地与 2026-07-31 解除日期",
      time: "14:18:14",
    },
    {
      sequence: 3,
      eventType: "ExecutionStarted",
      summary: "启动封顶标准复核",
      time: "14:18:16",
    },
    {
      sequence: 4,
      eventType: "SnapshotPinned",
      summary: "复用并验证 2026-07-15 冻结快照哈希",
      time: "14:18:16",
    },
    {
      sequence: 5,
      eventType: "NavigationRequested",
      summary: "发起全国封顶规则与深圳统计口径两条导航分支",
      time: "14:18:17",
    },
    {
      sequence: 6,
      eventType: "NavigationResultsAccepted",
      summary: "接受 2 条导航结果并选择 3 份文档",
      time: "14:18:18",
    },
    {
      sequence: 7,
      eventType: "SegmentReadRequested",
      summary: "读取《劳动合同法》第四十七条封顶规则",
      time: "14:18:19",
    },
    {
      sequence: 8,
      eventType: "EvidenceAccepted",
      summary: "接受第四十七条第二款逐字证据",
      time: "14:18:20",
    },
    {
      sequence: 9,
      eventType: "SegmentReadRequested",
      summary: "读取实施条例第二十七条工资构成规则",
      time: "14:18:21",
    },
    {
      sequence: 10,
      eventType: "EvidenceAccepted",
      summary: "接受第二十七条逐字证据；累计完成 4 次片段读取",
      time: "14:18:23",
    },
    {
      sequence: 11,
      eventType: "ModelRequestIssued",
      summary: "请求从深圳统计发布页第 5 个目标片段抽取候选证据",
      time: "14:18:24",
    },
    {
      sequence: 12,
      eventType: "ModelTransportInterrupted",
      summary: "模型服务返回 HTTP 503；判定为可重试传输错误",
      time: "14:18:26",
    },
    {
      sequence: 13,
      eventType: "ExecutionCheckpointed",
      summary: "保留 Running 检查点与全部已提交计数，未写入失败终态",
      time: "14:18:26",
    },
  ],
  updatedAt: "2026-07-18T14:18:26+08:00",
};

const conversations: ResearchConversation[] = [
  {
    id: "conversation-compensation-baseline",
    title: "劳动合同解除补偿研究",
    requests: [normalRequest],
  },
  {
    id: "conversation-compensation-recovery",
    title: "深圳封顶标准复核（待恢复）",
    requests: [recoveryRequest],
  },
];

export const demoFixtures: DemoFixtures = {
  snapshots: [
    currentLaborSnapshot,
    previousLaborSnapshot,
    archivedCaseSnapshot,
  ],
  conversations,
  normalRequest,
  recoveryRequest,
};

export default demoFixtures;
