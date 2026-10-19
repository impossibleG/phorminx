# Release notes

One file per release, named `v<version>.md`. `scripts/Publish-PhorminxRelease.ps1`
uses it as the GitHub Release body and refuses to publish without it, and
phorminx.net builds its changelog page from these files at the latest release tag.

Keep this shape so the website can read it:

```markdown
# Phorminx 0.2.3

Released 2026-10-18

One or two sentences on what this release is about.

## New

- **Feature name**: what a user can now do.

## Better

- Something that works better than before.

## Fixed

- A bug, described by what the user saw.
```

Leave out a section that has nothing in it. Write for users, not for
developers: no PR numbers, file names or internal module names. Write the file
before tagging, in the same pull request as the version bump.
