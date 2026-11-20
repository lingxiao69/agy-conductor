use crate::orchestrator::types::{AgentSlot, GroupChatMessage, RoleKind, TaskItem};

fn format_team_roster(slots: &[AgentSlot]) -> String {
    let mut lines = Vec::new();
    for s in slots.iter().filter(|s| s.enabled) {
        lines.push(format!(
            "- @{} | {} | 角色: {} | 优先级: P{} | 模型: {}",
            s.alias,
            s.display_name,
            s.role.label_zh(),
            s.normalized_priority(),
            s.model_id
        ));
    }
    lines.join("\n")
}

fn format_recent_chat(messages: &[GroupChatMessage], limit: usize) -> String {
    let start = messages.len().saturating_sub(limit);
    let mut out = Vec::new();
    for m in &messages[start..] {
        out.push(format!(
            "[{}] {} (@{}): {}",
            m.timestamp, m.sender_display, m.sender_alias, m.content
        ));
    }
    if out.is_empty() {
        "(暂无历史群消息)".to_string()
    } else {
        out.join("\n")
    }
}

fn format_tasks_overview(tasks: &[TaskItem]) -> String {
    if tasks.is_empty() {
        return "(尚未拆解子任务)".to_string();
    }
    tasks
        .iter()
        .map(|t| {
            format!(
                "- {} [{}] {:?} | 归属路径: {:?} | 依赖: {:?} | 返工次数: {}",
                t.task_id, t.title, t.status, t.owned_paths, t.depends_on, t.rework_count
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn build_architect_planning_prompt(
    goal: &str,
    workspace: &str,
    slots: &[AgentSlot],
    recent_chat: &[GroupChatMessage],
    existing_tasks: &[TaskItem],
    extra_directive: Option<&str>,
) -> String {
    let roster = format_team_roster(slots);
    let chat = format_recent_chat(recent_chat, 12);
    let tasks = format_tasks_overview(existing_tasks);
    let extra = extra_directive
        .map(|d| format!("\n## 特别指令 / 变更上下文\n{d}\n"))
        .unwrap_or_default();

    format!(
        r#"你是 AgyConductor 研发群组的【总架构规划师 (Architect)】。
请勘察当前项目目录 `{workspace}`，基于用户长期目标制定清晰的架构契约与无路径冲突的子任务 DAG 清单。

## 团队成员（按优先级数字从小到大调度）
{roster}

## 用户长期目标
{goal}

## 当前子任务状态
{tasks}

## 近期群聊记录
{chat}
{extra}
## 高效轻量勘察守则（防超时硬约束）
1. 只读勘察仓库结构与现有代码，**必须将规划阶段控制在 12~18 步工具操作内完成**，切勿过度消耗时间导致超时。
2. 优先列出目录树、检索关键符号、阅读模块入口文件与公共接口/数据结构定义来掌握全局架构；**严禁在规划阶段逐字通读全仓所有实现细节与完整测试文件**，详细阅读与修改交给具体子任务的编码程序员完成。
3. 不要替程序员写大量实现代码。

## 执行纪律
1. 每个子任务必须声明清晰的 `owned_paths`（目录或文件路径前缀，互不重叠则支持 P1/P2 并行编码）、`depends_on` 与可验证的 `acceptance_criteria`。
2. 规划完成后返回 `action: "DISPATCH_PLAN"`，在 `chat_message` 中以自然团队语气说明方案并 `@对应程序员别名` 派单。
3. 遇到技术性接口冲突、路径边界碰撞或返工卡点时，**优先由你在技术层面直接拍板调整方案并派单**，不要动辄打扰项目负责人。
4. 仅当任务存在无法自行判断的重大业务歧义或高风险产品取舍时，才返回 `NEED_USER_CLARIFICATION`。
5. 若用户的长期目标明确只需总架构师只读审查、分析项目或直接给出结论（无需修改代码或派发程序员），则在勘察后直接返回 `action: "FINAL_ACCEPT"`、`tasks: []`，并在 `chat_message` 中输出完整审查结论。

## 强制“通俗大白话”沟通规范（向用户提问或回复用户时必守）
当你返回 `action: "NEED_USER_CLARIFICATION"` 或回应项目负责人的群聊消息时，`chat_message` 与 `user_plain_explanation` 必须面向**不懂底层代码细节的项目负责人**使用日常大白话：
- **严禁堆砌底层代码黑话**（禁止直接抛 `E0063`、编译器报错码、结构体名、函数签名或冗长文件路径）；
- **必须清晰讲明三件事**：① 现在卡在哪个具体功能点上；② 通俗大白话原因（例如“两个功能的改动范围撞车了，直接改可能影响旧功能”）；③ 给出编号清晰的决策选项（例如“回复 1：保持旧方式不变，只做兼容扩展；回复 2：允许把关联模块一起改掉”），让负责人一眼就能拍板！
- 严格按照约定的 JSON Schema 返回最终结果。"#
    )
}

pub fn build_architect_arbitration_directive(
    task_id: &str,
    reason: &str,
    round: u8,
    max_rounds: u8,
) -> String {
    format!(
        "【第 {round}/{max_rounds} 次架构自动优先裁决】子任务 {task_id} 触发阻塞：{reason}\n\
         要求：\n\
         1. 请你优先在技术层面直接拍板解决（如调整 `owned_paths` 归属边界、选定兼容的公共接口方案、合并或重拆子任务），返回 `action: \"DISPATCH_PLAN\"` 让程序员无需人工打扰即可自动恢复干活。\n\
         2. 仅当你审慎评估后确认涉及业务需求歧义或高风险产品取舍、确实无法替用户做主时，才返回 `action: \"NEED_USER_CLARIFICATION\"`，并务必在 `user_plain_explanation` 与 `chat_message` 中用通俗大白话讲清卡住的功能、原因和带编号的选择题（如回复 1 / 回复 2）。"
    )
}

pub fn build_user_interjection_directive(notes: &[String]) -> String {
    format!(
        "项目负责人（用户）在运行中途发来最新指导/拍板意见：\n- {}\n\
         请在不推倒已验收成果的前提下，第一时间按用户要求调整、解锁或补充子任务，并用通俗大白话简明回复确认，随后返回 `action: \"DISPATCH_PLAN\"` 继续推进。",
        notes.join("\n- ")
    )
}

pub fn build_architect_final_gate_prompt(
    goal: &str,
    workspace: &str,
    slots: &[AgentSlot],
    tasks: &[TaskItem],
    recent_chat: &[GroupChatMessage],
    verified_commands: &[String],
) -> String {
    let roster = format_team_roster(slots);
    let tasks_summary = format_tasks_overview(tasks);
    let chat = format_recent_chat(recent_chat, 15);
    let cmds = if verified_commands.is_empty() {
        "(无记录)".to_string()
    } else {
        verified_commands.join(", ")
    };

    format!(
        r#"你是 AgyConductor 研发群组的【总架构规划师 (Architect)】，当前进入【全局终验门禁 (Final Gate)】。
所有已派发子任务均已通过独立代码审查员验收，请你核对最初的「长期目标」是否 100% 达成、各模块集成是否无缝。

## 项目目录
{workspace}

## 原始长期目标
{goal}

## 团队成员
{roster}

## 已验收子任务清单
{tasks_summary}

## 审查员已验证命令
{cmds}

## 近期群聊记录
{chat}

## 终验判定要求
1. 请快速检查关键产物、端到端集成和验证命令结果，确认没有任何遗漏项、未接线入口或 TODO 占位。
2. 若确认 100% 达成目标：返回 `action: "FINAL_ACCEPT"`，`tasks: []`，并在 `chat_message` 中撰写清晰简洁的交付验收总结。
3. 若发现跨模块拼接缺口或遗漏需求：返回 `action: "DISPATCH_PLAN"`，仅在 `tasks` 中追加新的收尾子任务（使用新的 `task_id`），并 `@程序员` 继续收口。
4. 严格按照 JSON Schema 输出。"#
    )
}

pub fn build_coder_prompt(
    goal: &str,
    architecture_notes: &str,
    task: &TaskItem,
    coder_slot: &AgentSlot,
    reviewer_alias: &str,
    architect_alias: &str,
    shared_build_lock: bool,
    is_rework: bool,
    is_handoff: bool,
) -> String {
    let criteria = task
        .acceptance_criteria
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}", i + 1, c))
        .collect::<Vec<_>>()
        .join("\n");

    let build_hint = if shared_build_lock {
        "注意：当前仓库存在共享构建锁文件（如 Cargo.toml / package.json），并发施工时优先针对你负责的模块运行轻量静态检查或定向单测，由审查员统一串行跑全量集成测试。"
    } else {
        "完成代码后请运行局部自测确认无编译和逻辑错误。"
    };

    let rework_section = if is_rework {
        let issues_text = task
            .last_review_issues
            .iter()
            .enumerate()
            .map(|(i, iss)| {
                let loc = iss
                    .line_hint
                    .as_deref()
                    .map(|l| format!("{}:{}", iss.file_path, l))
                    .unwrap_or_else(|| iss.file_path.clone());
                format!("{}. [{}] {} — {}", i + 1, iss.severity, loc, iss.detail)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let handoff_note = if is_handoff {
            format!(
                "\n【跨模型接力说明】原程序员暂时不可用，现由你接力修复该任务。原实现摘要：{}\n原修改文件：{:?}\n请先查看 `git diff` 与上述文件后再精准修复。",
                task.last_coder_summary.as_deref().unwrap_or("(无)"),
                task.last_modified_files
            )
        } else {
            String::new()
        };
        format!(
            r#"
## 审查打回修改清单（第 {} 轮返工）
审查员说明：{}
逐项缺陷：
{}
{}
请逐一彻底修复上述全部问题并补充必要的回归验证。"#,
            task.rework_count,
            task.last_review_note.as_deref().unwrap_or("请按缺陷清单修复"),
            if issues_text.is_empty() {
                "(见审查员说明)".to_string()
            } else {
                issues_text
            },
            handoff_note
        )
    } else {
        String::new()
    };

    format!(
        r#"你是 AgyConductor 研发群组的【编码程序员 (Coder)】：{} (@{}，优先级 P{})。

## 项目长期目标
{goal}

## 架构契约与工程红线
{architecture_notes}

## 当前分配子任务：{} — {}
任务详情：
{}

归属修改路径（严禁越界修改其他任务负责的文件）：
{:?}

验收标准：
{}
{}
## 工作要求
1. 严格遵守所有工程红线（不留 TODO/占位符、编码 UTF-8 无 BOM、模块化清晰）。
2. {build_hint}
3. 完成并自测通过后返回 `status: "SUBMIT_FOR_REVIEW"`，在 `chat_message` 中简明汇报变更并 `@{reviewer_alias}` 提请代码审查。
4. 仅当架构设计存在根本冲突无法落地时，返回 `status: "BLOCKED_BY_ARCH"` 并填写 `blocker_reason` 同时 `@{architect_alias}`。
5. 严格按照 JSON Schema 输出。"#,
        coder_slot.display_name,
        coder_slot.alias,
        coder_slot.normalized_priority(),
        task.task_id,
        task.title,
        task.description,
        task.owned_paths,
        criteria,
        rework_section
    )
}

pub fn build_reviewer_prompt(
    goal: &str,
    architecture_notes: &str,
    verification_commands: &[String],
    task: &TaskItem,
    coder_alias: &str,
) -> String {
    let criteria = task
        .acceptance_criteria
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {}", i + 1, c))
        .collect::<Vec<_>>()
        .join("\n");
    let cmds = if verification_commands.is_empty() {
        "请根据项目类型自行运行合适的构建、单测与静态检查命令。".to_string()
    } else {
        verification_commands.join("\n- ")
    };

    format!(
        r#"你是专职代码审查员，只能读取文件、查看 `git diff`、运行构建/测试/行数检查命令，严禁亲自编辑或修改业务源码！发现任何未实现功能、测试失败、边界漏洞或违反要求，必须返回 `CHANGES_REQUESTED` 并详细指出修改位置打回给程序员。

## 项目长期目标
{goal}

## 架构契约与红线
{architecture_notes}

## 待审查子任务：{} — {}
提交程序员：@{coder_alias}
当前返工轮次：{}
任务描述：{}
归属路径：{:?}
程序员汇报：{}
程序员修改文件：{:?}
程序员自测结果：{}

## 验收标准
{}

## 建议验证命令
- {cmds}

## 审查输出纪律
1. 严格核对 `git diff`、修改文件、边界条件、错误处理、测试覆盖与验收标准。
2. 任何缺漏、失败测试、未实现占位或越界修改，必须返回 `verdict: "CHANGES_REQUESTED"`，在 `issues` 中写明 `severity`、`file_path`、`line_hint`、`detail`，并在 `chat_message` 中 `@{coder_alias}` 说明需要修改的点。
3. 全部达标且验证命令通过后，返回 `verdict: "APPROVED"`，列出实际运行的 `commands_verified`，在 `chat_message` 中向群组宣布该子任务验收通过。
4. 严格按照 JSON Schema 输出。"#,
        task.task_id,
        task.title,
        task.rework_count,
        task.description,
        task.owned_paths,
        task.last_coder_summary.as_deref().unwrap_or("(无)"),
        task.last_modified_files,
        task.last_self_check.as_deref().unwrap_or("(无)"),
        criteria
    )
}

pub fn pick_role_alias(slots: &[AgentSlot], role: RoleKind, fallback: &str) -> String {
    slots
        .iter()
        .filter(|s| s.enabled && s.role == role)
        .min_by_key(|s| s.normalized_priority())
        .map(|s| s.alias.clone())
        .unwrap_or_else(|| fallback.to_string())
}
