---
name: restricted-writer
description: A sample subagent whose tools list excludes Skill.
tools: Read, Edit, Write, Bash
---

A subagent definition with a restricted `tools` list that does not include the
`Skill` tool, so the enforcement gate (§3.3) exempts it.
