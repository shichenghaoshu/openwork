# GitHub project setup

The repository, labels, milestones, and canonical Issues #1–#30 were created on
2026-08-10. The M1 milestone additionally tracks Issues #3, #6, #12–#15, and
#63–#65. GitHub Project creation is pending because the current CLI token lacks
`project` and `read:project` scopes.

```bash
gh auth refresh -s project,read:project
./scripts/bootstrap-github.sh
```

The script creates or reuses `OpenWork Roadmap`, creates the prompt-defined fields,
and adds Issues #1–#30. Afterward, configure the `main` ruleset in repository settings:

- block force-push and branch deletion;
- require pull requests, required status checks, resolved conversations, and linear history;
- protect release tags;
- do not require a fabricated second-person review while only one maintainer exists.

Required check names are defined in `.github/workflows/ci.yml`.
