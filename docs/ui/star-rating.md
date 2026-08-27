# Star rating native contract

## Journey and workload

The first call site is the canonical book-details editor. A reader can set or clear a personal
zero-to-five-star rating in exact half-star steps, then persist it with the section's ordinary Save
action. The representative release workload is `ui-book-detail-regression-v2`, whose deterministic
fixture renders a 3.5-star value alongside publication metadata and retains open-to-painted-detail
p95, first-paint, peak-RSS, and correctness samples.

## Supported API

- `StarRating::new(id, half_stars)` accepts an exact bounded value from `0..=10`.
- `disabled` prevents pointer and keyboard activation while a metadata operation is active.
- `on_change` reports one of the ten non-zero half-star targets. The application owns clearing,
  dirty state, persistence, validation, and any visible numeric description.

## Presentation and accessibility

Five stars form one compact row. Each visible star overlays two equal activation targets so values
such as 0.5, 2.5, and 4.5 do not require a secondary control. Filled, empty, disabled, and focus
colors come from Lectern-owned rating and focus tokens. Every half-star target is a native button
with a full value label and Enter/Space activation; the product call site also shows `Unrated` or
the exact value out of five.

## Performance contract

The component renders a fixed five stars and ten activation targets with no retained component
state, heap collection, storage access, or unbounded work. Its application value remains an exact
integer half-star count.
