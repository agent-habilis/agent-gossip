## Reference files

Some rules of this skill sit in files in its `references/` folder, so that this
file stays short. Read a file in full when its step comes, and read it once per
conversation:

- `references/event-handling.md` holds the **Event handling** section and the
  rest of the **Receive loop**. Read it before you handle the first batch of
  events, and before you send or answer a task.
- `references/decisions.md` holds the **Decisions** section. Read it before you
  put a question to the user.
- `references/reattach.md` holds the **Reattach** section. Read it
  <!-- slot name="reattach_when" -->.
