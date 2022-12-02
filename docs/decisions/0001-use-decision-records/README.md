---
status: accepted
date: 2022-12-01
deciders: ['@cbandy']

---
# Use Decision Records

## Context and Problem Statement

We want to record decisions made in this project to document its current design and inform future decisions.
The records should provide context, including the date and links to related decisions or documentation.
We want to commit these records to version control so they survive as long as the code.
What format or structure should these records follow?

## Considered Options

- [MADR](https://adr.github.io/madr/) 3.0.0 – Markdown Any Decision Records
- Michael Nygard's template – The first incarnation of the term "ADR"
- Y-Statements
- Kubernetes Enhancement Proposals, [KEPs](https://github.com/kubernetes/enhancements/blob/44e075152fe0/keps/README.md)
- No conventions for file format nor structure

## Decision Outcome

We will use the "MADR" format in numbered directories so that each record can incorporate images or other data when desired.

- GitHub presents a browsable index of records with links to their rendered Markdown.
- Structured data can live in front matter or a separate file without breaking links.
  ([Jekyll][fm-jekyll], [Hugo][fm-hugo])
- Sequential numbering is sufficient and can be coordinated just before we merge a record.

[fm-hugo]: https://gohugo.io/content-management/front-matter/
[fm-jekyll]: https://jekyllrb.com/docs/front-matter/
