//! 팀원 업무 기억 (스펙 v0.2 4.3, A단계 5번, 최소 범위).
//!
//! - 팀원마다 짧은 요약 노트 하나. 작업할 때 시스템 프롬프트에 넣는다 (크기 상한 = 매 실행 비용 상한).
//! - 팀원이 작업 보고 끝에 적은 "기억할 점"만 모델 호출 없이 골라 노트에 붙인다.
//! - 사장(사용자)이 노트를 보고 고칠 수 있다. 틀린 노트는 틀린 작업으로 이어지므로.

use rusqlite::{Connection, params};

use crate::error::AppResult;

/// 노트 최대 길이 (넘으면 오래된 줄부터 지운다)
pub const MAX_MEMORY_CHARS: usize = 2000;

/// 팀원에게 주는 안내 (시스템 프롬프트에 덧붙임)
pub const MEMORY_GUIDE: &str = "작업을 마칠 때 다음 작업에 도움이 될 내용(결정 사항, 사장의 선호, 주의할 점)이 있으면 \
마지막에 '## 기억할 점' 제목 아래 짧은 목록으로 적으세요. 없으면 적지 않습니다.";

/// 보고에서 "기억할 점" 목록을 뽑는다. 제목(## 기억할 점) 아래 목록 또는 `- 기억할 점: 내용` 한 줄 형식.
pub fn extract(report: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in report.lines() {
        let t = line.trim();
        let heading = t.trim_start_matches('#').trim();
        if t.starts_with('#') {
            in_section = heading.starts_with("기억할 점");
            continue;
        }
        let bullet = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("• "));
        if in_section {
            match bullet {
                Some(b) if !b.trim().is_empty() => out.push(b.trim().to_string()),
                None if !t.is_empty() => in_section = false,
                _ => {}
            }
        } else if let Some(b) = bullet
            && let Some(v) = b
                .strip_prefix("기억할 점:")
                .or_else(|| b.strip_prefix("기억할 점 :"))
        {
            let v = v.trim();
            if !v.is_empty() {
                out.push(v.to_string());
            }
        }
    }
    out.retain(|x| !matches!(x.trim_end_matches('.'), "없음" | "없습니다" | "해당 없음"));
    out
}

/// 기존 노트에 새 항목을 붙인다 (중복 제외). 길면 오래된 줄부터 지운다.
pub fn merge(existing: &str, items: &[String]) -> String {
    let mut lines: Vec<String> = existing
        .lines()
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .collect();
    for it in items {
        let line = format!("- {it}");
        if !lines.iter().any(|l| l.trim() == line.trim()) {
            lines.push(line);
        }
    }
    while lines.join("\n").chars().count() > MAX_MEMORY_CHARS && lines.len() > 1 {
        lines.remove(0);
    }
    lines.join("\n")
}

pub fn get(conn: &Connection, agent_id: i64) -> AppResult<String> {
    Ok(
        conn.query_row("SELECT memory FROM agents WHERE id = ?1", [agent_id], |r| {
            r.get(0)
        })?,
    )
}

pub fn set(conn: &Connection, agent_id: i64, memory: &str) -> AppResult<()> {
    let trimmed = merge(memory, &[]);
    conn.execute(
        "UPDATE agents SET memory = ?1 WHERE id = ?2",
        params![trimmed, agent_id],
    )?;
    Ok(())
}

/// 작업 보고에서 기억할 점을 노트에 반영한다. 반영했으면 true.
pub fn absorb(conn: &Connection, agent_id: i64, report: &str) -> AppResult<bool> {
    let items = extract(report);
    if items.is_empty() {
        return Ok(false);
    }
    let merged = merge(&get(conn, agent_id)?, &items);
    conn.execute(
        "UPDATE agents SET memory = ?1 WHERE id = ?2",
        params![merged, agent_id],
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 기억할_점_추출() {
        let report = "## 작업 보고\n- 결과: 완료\n- 요약: 고침\n\n## 기억할 점\n- 보고서는 표 형식을 선호함\n- 날짜는 YYYY-MM-DD\n\n다른 문단";
        assert_eq!(
            extract(report),
            vec!["보고서는 표 형식을 선호함", "날짜는 YYYY-MM-DD"]
        );
        let inline = "## 작업 보고\n- 결과: 완료\n- 기억할 점: 고객사 이름은 영문 표기\n";
        assert_eq!(extract(inline), vec!["고객사 이름은 영문 표기"]);
        assert!(extract("## 기억할 점\n- 없음\n").is_empty());
        assert!(extract("기억할 점 없이 끝난 보고").is_empty());
    }

    #[test]
    fn 합치기_중복제외_길이상한() {
        let m = merge("- a", &["a".into(), "b".into()]);
        assert_eq!(m, "- a\n- b");
        let long: Vec<String> = (0..300)
            .map(|i| format!("항목 {i} 내용이 조금 긴 기억"))
            .collect();
        let m = merge("", &long);
        assert!(m.chars().count() <= MAX_MEMORY_CHARS);
        assert!(
            m.ends_with("항목 299 내용이 조금 긴 기억"),
            "최근 것이 남는다"
        );
    }
}
