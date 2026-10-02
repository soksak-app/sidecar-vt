# Features

[한국어](features.ko.md)

- [o] S1 — P1: Build, test and release this sidecar as its own repository. Moved on 2026-10-02 from the soksak core repository (checklist item R1-5-2 there), whose history holds the earlier changes. `make test`, `make build` and `make release` pass on macOS arm64.
- [o] S2 — P1: Open a surface again after its closed notice on the same persistent connection. Found on 2026-10-02 by the core window check `library windows create and open projects in place`: a project whose terminal was shown in a window that then closed could not be shown in a new window, because the terminal open was answered with `stale attachment` and the host waited for a terminal image that never came. The closed notice removed the registry entry but left the surface's sender and attachment epoch in the connection, so the next open compared the old epoch with a registry that no longer had the entry. Done on 2026-10-02: a successful close also removes them. Red: `a_surface_reopened_after_its_closed_notice_is_not_a_stale_attachment` received two `stale attachment` replies; Green: the test and `make test` pass on macOS arm64.
