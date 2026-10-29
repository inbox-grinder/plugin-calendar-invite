# Calendar invitations

This processor reads approved calendar MIME content, not just embedded body text.
`fixtures/invite.ics` is carried in `ics-input.json`; `text-input.json` exercises
labelled text fallback. Both describe the same event and produce the same identity
and semantic update. Event title, start/end instants, and location carry retained
email evidence. Repeated invitations keep their update key; changed event facts
create a new update under the UID identity.

Only single non-recurring UTC events are supported in structured calendars.
Floating times, named zones, all-day events, recurrence, cancellations, multiple
events, invalid dates and duplicate required properties remain ordinary mail.
Text fallback requires explicit offset times and Event ID, Event/Title, Start,
End and Location labels. Unlabelled prose remains ordinary mail.

The host requires approved `read_calendar_attachments` permission and the calendar
content need. See `docs/plugin-api.md` for MIME filtering and byte limits.
