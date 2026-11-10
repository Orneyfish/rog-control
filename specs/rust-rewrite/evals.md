# Evals: Rust rewrite of ROG Control

Approved by the user before building. Nobody edits this file while building; outcomes go in results.md.

Reference implementation: the Python app at git commit `509d5e4`. "Parity" below always means "same as that commit, observed the same way".

## Setup
This app has no network port. Isolation comes from: a throwaway `$HOME` holding a copy of the config, a private D-Bus session bus (`dbus-run-session`) for everything except the two visual evals, and fake hardware tools on `PATH` that log every call and let only read-only calls through (`harness/fake-tools.sh`, which fails closed, so a write or `systemctl reboot` can never reach the machine). Windows opened by these evals appear on the live Hyprland session.

1. Work is committed on branch `feat/rust-rewrite` (the install evals copy tracked files only).
```
cd /home/orneyfish/Projects/rog-control && git switch feat/rust-rewrite && git status --porcelain | grep -v '^?? .claude/'   # must print nothing
export REPO=$PWD H0=$HOME CARGO_HOME=$HOME/.cargo RUSTUP_HOME=$HOME/.rustup
cargo build --release --locked && export BIN=$REPO/target/release/rog-control
. specs/rust-rewrite/harness/fake-tools.sh        # sets and exports T (scratch dir), FAKE, FAKE_LOG
. specs/rust-rewrite/harness/lib.sh               # wait_name ping_svc ctl win wait_win imgsame sizeok closewin app
specs/rust-rewrite/harness/hw-snapshot.sh > $T/hw.before
export H=$T/home; mkdir -p $H/.config/rog-control $H/.local/state/caelestia $T/empty $T/refshots $T/rust
cp ~/.config/rog-control/config.json $H/.config/rog-control/        # a COPY; the real file is never an eval target
cp ~/.local/state/caelestia/scheme.json $H/.local/state/caelestia/
mkdir -p $H/.config/hypr/custom && cp ~/.config/hypr/custom/rules.lua $H/.config/hypr/custom/   # the ported rules-syntax tests read it from $HOME
sha256sum ~/.config/rog-control/config.json > $T/realcfg.sha
git worktree add $T/ref 509d5e4                    # the Python reference, runnable as `python3 $T/ref/main.py`
systemctl --user stop rog-control.service          # only so the visual evals can own the app id; restarted in E21
```
2. Every "private bus" eval writes its script to `$T/eN.sh` and runs it as `timeout 180 dbus-run-session -- bash $T/eN.sh` from `$REPO` (all variables above stay exported). Do not pipe that command into `tail` or `grep`: a stray background child can keep the pipe open and make the pipeline hang (seen while dry-running this harness). `app` starts the binary with the fake tools on `PATH` and `HOME=$H`.
3. Scripts print one line per observation; `FAIL` anywhere in the output fails the eval.

## E1: One Rust binary builds cleanly, no Python inside
- covers: FR-1, NFR-4
- check: `cargo build --release --locked && ls -l target/release/rog-control && file target/release/rog-control && ldd target/release/rog-control | grep -ci python; git ls-files Cargo.lock; cargo clippy --all-targets --locked -- -D warnings; echo clippy=$?; grep -cE '^(tokio|async-std|smol|pyo3|cpython)' Cargo.toml`
- pass when: the build exits 0; `file` says ELF executable; the python grep prints `0`; `Cargo.lock` is listed as tracked; clippy exits 0; the last grep prints `0` (no async runtime, no Python binding).
- fail when: the build needs network access it does not have, links libpython or embeds an interpreter, ships without a lock file, has clippy errors, or pulls in an async runtime for a handful of D-Bus calls.

## E2: Starts hidden, answers fast, exposes the control API
- covers: FR-2, FR-3, FR-6, NFR-2, NFR-5
- check: write and run this as an `E2` private-bus script:
```
. specs/rust-rewrite/harness/lib.sh
for run in 1 2 3; do
  t0=$(date +%s%N); app >$T/e2.out 2>>$T/e2.err & pid=$!
  wait_name $CTL_NAME 5 && ping_svc 5 >/dev/null || { echo "FAIL not up on run $run"; exit 1; }
  echo "run $run ready in $(( ($(date +%s%N) - t0) / 1000000 )) ms"
  [ "$(win 'ROG Control')" = 0 ] && echo "no window at boot" || echo "FAIL window shown at boot"
  busctl --user introspect $CTL_NAME $CTL_PATH $CTL_NAME --no-pager | awk '/ method /{print $1}' | sort | tr '\n' ' '; echo
  kill $pid; wait $pid 2>/dev/null
done
echo "stdout bytes: $(wc -c < $T/e2.out)  panics: $(grep -c 'panicked at' $T/e2.err)"
```
- pass when: all three runs print `ready in` a value of at most 5000 ms; each prints `no window at boot`; the method line is exactly `.CycleProfile .CycleProfileBack .Open .OpenWindow` (the `org.freedesktop.DBus.*` members are in other interfaces); `stdout bytes: 0  panics: 0`.
- fail when: the service never claims the name or never answers `Peer.Ping` (blocked main loop), a window pops up at boot, a method is missing or misnamed, anything is printed to stdout, or it panics.

## E3: Hostile environment: corrupt config, no tools, hanging tools
- covers: FR-26, FR-29, NFR-3
- check: write and run as an `E3` private-bus script:
```
. specs/rust-rewrite/harness/lib.sh
# a) corrupt config tolerated and not rewritten
mkdir -p $T/h3/.config/rog-control; printf '{not json' > $T/h3/.config/rog-control/config.json
before=$(sha256sum < $T/h3/.config/rog-control/config.json)
APPHOME=$T/h3 app 2>$T/e3a.err & pid=$!
wait_name $CTL_NAME 5 && ping_svc 5 >/dev/null && echo "a) up with corrupt config" || echo "FAIL a) not up"
sleep 2; [ "$(sha256sum < $T/h3/.config/rog-control/config.json)" = "$before" ] && echo "a) config untouched" || echo "FAIL a) rewritten at startup"
kill $pid; wait $pid 2>/dev/null
# b) no hardware tools on PATH at all (and so no python either)
env PATH=$T/empty HOME=$H "$BIN" 2>$T/e3b.err & pid=$!
wait_name $CTL_NAME 5 && ctl Open >/dev/null && wait_win 'ROG Control' 1 3 && echo "b) window shown with no tools" || echo "FAIL b)"
sleep 2; kill -0 $pid && echo "b) still alive" || echo "FAIL b) died"
kill $pid; wait $pid 2>/dev/null
# c) every tool call hangs for 8 s: the GTK main loop must never wait on them
FAKE_SLEEP=8 app 2>$T/e3c.err & pid=$!
wait_name $CTL_NAME 5 && ping_svc 1 >/dev/null && echo "c) answers while tools hang" || echo "FAIL c) startup blocked"
ctl Open >/dev/null; wait_win 'ROG Control' 1 2 && echo "c) window within 2s" || echo "FAIL c) window blocked"
ping_svc 1 >/dev/null && echo "c) answers with window open" || echo "FAIL c) blocked after open"
kill $pid; wait $pid 2>/dev/null
echo "panics: $(cat $T/e3?.err | grep -c 'panicked at')"
```
- pass when: every `a)`, `b)`, `c)` line is the non-FAIL one and `panics: 0`.
- fail when: a corrupt config aborts startup or gets overwritten, a missing tool panics or hides the window, or the process calls a tool synchronously on the main thread (the 8 s hang makes the ping, the window or both miss their deadline).

## E4: The flags drive the running service; soft failure without one
- covers: FR-3, FR-4, FR-16
- check: write and run as an `E4` private-bus script:
```
. specs/rust-rewrite/harness/lib.sh
next() { case $1 in quiet) echo Balanced;; balanced) echo Performance;; *) echo Quiet;; esac; }
prev() { case $1 in quiet) echo Performance;; balanced) echo Quiet;; *) echo Balanced;; esac; }
plan() { case $1 in Quiet) echo power-saver;; Balanced) echo balanced;; *) echo performance;; esac; }
s=$(date +%s); "$BIN" --open 2>$T/e4.err; echo "no service: rc=$? in $(( $(date +%s) - s ))s msg=[$(cat $T/e4.err)]"
app 2>$T/e4svc.err & pid=$!; wait_name $CTL_NAME 5
"$BIN" --open; echo "open rc=$?"; wait_win 'ROG Control' 1 3 && echo shown || echo "FAIL not shown"
"$BIN" --open; wait_win 'ROG Control' 0 3 && echo hidden || echo "FAIL not hidden"
cur=$(cat /sys/firmware/acpi/platform_profile)
: > $FAKE_LOG; "$BIN" --cycle-profile; sleep 1
echo "forward from $cur:"; grep -E '^(asusctl profile set|powerprofilesctl set)' $FAKE_LOG
echo "expect: asusctl profile set $(next $cur) + powerprofilesctl set $(plan $(next $cur))"
: > $FAKE_LOG; "$BIN" --cycle-profile-back; sleep 1
echo "back from $cur:"; grep -E '^(asusctl profile set|powerprofilesctl set)' $FAKE_LOG
echo "expect: asusctl profile set $(prev $cur) + powerprofilesctl set $(plan $(prev $cur))"
kill $pid; wait $pid 2>/dev/null
```
- pass when: `no service: rc=0 in` at most 5 s with a message containing `ROG Control service not reachable`; `open rc=0`, `shown`, `hidden`; after each cycle the two logged lines are exactly the two shown on the `expect:` line below them, in either order, and nothing else was written.
- fail when: the flag launch hangs or exits non-zero with no service, the toggle only ever opens, cycling goes the wrong way, skips a profile, forgets the paired power plan, or sends extra writes.

## E5: Repeated launcher clicks leave exactly one process
- covers: FR-5
- check: write and run as an `E5` private-bus script:
```
. specs/rust-rewrite/harness/lib.sh
app 2>$T/e5.err & pid=$!; wait_name $CTL_NAME 5
for i in 1 2 3 4 5 6 7 8; do s=$(date +%s%N); timeout 5 "$BIN"; echo "launch $i rc=$? in $(( ($(date +%s%N) - s) / 1000000 )) ms"; done
sleep 1; echo "processes: $(pgrep -xc rog-control)"; kill -0 $pid && echo "first instance alive" || echo "FAIL first instance died"
ping_svc 2 >/dev/null && echo "still answers" || echo "FAIL not answering"
kill $pid; wait $pid 2>/dev/null
```
- pass when: all eight launches print `rc=0` in under 5000 ms, `processes: 1`, `first instance alive`, `still answers`.
- fail when: any launch exits 124 (hung), non-zero, or leaves a lingering second process (the bug that once left eight invisible instances), or the running instance dies.

## E6: Tray item: properties, late-watcher retry, click, scroll, quit
- covers: FR-8, FR-9
- check: write and run as an `E6` private-bus script:
```
. specs/rust-rewrite/harness/lib.sh
prop() { busctl --user get-property $u /StatusNotifierItem org.kde.StatusNotifierItem "$1"; }
# part 1: the watcher appears 4 s AFTER the app starts; registration must still happen (retry every 3 s)
app 2>$T/e6.err & pid=$!; wait_name $CTL_NAME 5; sleep 4
python3 specs/rust-rewrite/harness/fake-watcher.py $T/reg.txt & wpid=$!
for i in $(seq 1 100); do [ -s $T/reg.txt ] && break; sleep 0.1; done
echo "registered: $(cat $T/reg.txt)"
u=$(cut -d/ -f1 $T/reg.txt)
prop Id; prop Title; prop Category; prop Status; prop IconName; prop ItemIsMenu
busctl --user call $u /StatusNotifierItem org.kde.StatusNotifierItem Activate ii 0 0; wait_win 'ROG Control' 1 3 && echo "activate: shown" || echo "FAIL activate"
busctl --user call $u /StatusNotifierItem org.kde.StatusNotifierItem Activate ii 0 0; wait_win 'ROG Control' 0 3 && echo "activate: hidden" || echo "FAIL activate toggle"
: > $FAKE_LOG; busctl --user call $u /StatusNotifierItem org.kde.StatusNotifierItem Scroll is 1 vertical; sleep 1
echo "scroll:"; grep '^asusctl profile set' $FAKE_LOG
busctl --user call $u /StatusNotifierItem org.kde.StatusNotifierItem SecondaryActivate ii 0 0
for i in $(seq 1 30); do kill -0 $pid 2>/dev/null || break; sleep 0.1; done
kill -0 $pid 2>/dev/null && echo "FAIL still running after SecondaryActivate" || echo "secondary activate: app exited"
kill $wpid
```
- pass when: `registered:` shows `<unique name>/StatusNotifierItem` (so the late watcher was found); the properties are `s "rog-control"`, `s "ROG Control"`, `s "Hardware"`, `s "Active"`, `s "rog-control-symbolic"`, `b false`; both activate lines are the non-FAIL ones; scroll logs one `asusctl profile set <next profile>`; the app exits within 3 s of `SecondaryActivate`.
- fail when: it gives up if the watcher is late, a property is wrong, left-click only opens, scroll does nothing or goes backward, or right-click leaves the process running.

## E7: Main window matches the reference (look, size, labels, clean stderr)
- covers: FR-10, FR-11, FR-12, FR-15, NFR-6
- check: this runs on the real session bus with real `$HOME` (read-only: the fake tools block every write), the real service stopped in Setup. Reference and candidate are captured the same way in the same session so live values match:
```
. specs/rust-rewrite/harness/lib.sh; export APPHOME=$H0
PATH=$FAKE:$PATH python3 specs/rust-rewrite/harness/shoot_ref.py $T/ref $T/refshots | tee $T/refshots/geom.txt   # ~12 s, shows the 4 Python windows one by one
app 2>$T/e7.err & pid=$!; wait_name $CTL_NAME 5; ctl OpenWindow s main >/dev/null; wait_win 'ROG Control' 1 3; sleep 2
specs/rust-rewrite/harness/shoot.sh 'ROG Control' $T/rust/ROG_Control.png
sizeok $T/rust/ROG_Control.png $T/refshots/ROG_Control.png && echo size-ok || echo "FAIL size"
imgsame $T/rust/ROG_Control.png $T/refshots/ROG_Control.png && echo look-ok || echo "FAIL look"
imgsame $T/refshots/ROG_Control.png $T/refshots/Extra.png >/dev/null && echo "FAIL control: different windows compared equal" || echo "control ok (a different window is rejected)"
echo "stderr offenders: $(grep -cE 'Theme parser|CRITICAL|panicked at' $T/e7.err)"
kill $pid; wait $pid 2>/dev/null
```
  Then open `$T/rust/ROG_Control.png` and `specs/rust-rewrite/ref/ROG_Control.png` side by side and tick off, in order: title `ROG Control` + device name; section headings Performance Mode, GPU Mode, Screen, Laptop Keyboard, Battery Charge Limit, Run on Startup; the buttons Silent, Balanced, Turbo, Fans + Power, Eco, Standard, Ultimate, Optimized, Auto, 60Hz, 240Hz + OD, Off, Low, Medium, High, Extra, the effect dropdown and colour swatch, the Full button, Firmware, Quit; every icon present; the live status labels (CPU temp / fan, GPU temp / fan, battery).
  One deliberate exception (user decision D16): the Keyboard effect dropdown and its colour swatch are expected to differ from the Python capture, because the Python app's hard-coded asusd path is stale on this machine and always showed `static`. In the Rust capture they must show the effect asusd reports right now. Read it with `busctl --system tree xyz.ljones.Asusd` (lists the `/xyz/ljones/aura/<id>` object), then `busctl --system get-property xyz.ljones.Asusd /xyz/ljones/aura/<id> xyz.ljones.Aura LedModeData` (read-only), map the mode index with `AURA_MODE_INDEX` in `src/hw.rs`, and name the effect you expect in results.md. If asusd reports `static` say so; then this exception is vacuous and E13 row 12 carries the proof.
- pass when: `size-ok`, `look-ok` (`imgsame` prints `AE3 RMSE_small` at most `0.015 0.02`), the control line says `control ok`, `stderr offenders: 0`, every item in the checklist is present, in the same place, with the same wording, and the Keyboard effect dropdown shows the effect asusd reports.
- fail when: the window is a different size, missing an icon or panel, mis-coloured, mis-spaced, an empty or placeholder window (the control proves the comparison rejects those), or GTK reports CSS problems.

## E8: Companion windows match the reference; open, close and reopen behave
- covers: FR-6, FR-10, FR-13, FR-15, NFR-6
- check: continues E7's shell (same variables, service still running on the real bus; if you restarted, redo its first three lines):
```
for row in "fans|Fans and Power|Fans_and_Power" "lighting|Keyboard Lighting|Keyboard_Lighting" "extra|Extra|Extra"; do
  IFS='|' read key title file <<<"$row"
  ctl OpenWindow s $key >/dev/null; wait_win "$title" 1 3 || echo "FAIL $title did not open"; sleep 2
  ctl OpenWindow s $key >/dev/null; sleep 1; echo "$title windows after second open: $(win "$title")"
  read w h x y addr <<<"$(specs/rust-rewrite/harness/shoot.sh "$title" $T/rust/$file.png)"
  sizeok $T/rust/$file.png $T/refshots/$file.png && imgsame $T/rust/$file.png $T/refshots/$file.png && echo "$title look-ok" || echo "FAIL $title look"
  echo "$title at x=$x y=$y   reference: $(grep "^shot $title " $T/refshots/geom.txt)"
  closewin $addr; wait_win "$title" 0 3 && echo "$title closed" || echo "FAIL $title did not close"
  ping_svc 2 >/dev/null && echo "service alive" || echo "FAIL service died after closing $title"
done
ctl OpenWindow s main >/dev/null; wait_win 'ROG Control' 1 3
read w h x y addr <<<"$(specs/rust-rewrite/harness/shoot.sh 'ROG Control' $T/rust/main2.png)"; closewin $addr
wait_win 'ROG Control' 0 3 && ping_svc 2 >/dev/null && echo "main close hides, service alive" || echo "FAIL main close quit the app"
ctl Open >/dev/null; wait_win 'ROG Control' 1 3 && echo "main reopens" || echo "FAIL main reopen"
ctl OpenWindow s fans >/dev/null; wait_win 'Fans and Power' 1 3; sleep 2          # reopen a companion that was closed above
read w h x y addr <<<"$(specs/rust-rewrite/harness/shoot.sh 'Fans and Power' $T/rust/fans2.png)"
imgsame $T/rust/fans2.png $T/refshots/Fans_and_Power.png && echo "reopened fans look-ok" || echo "FAIL reopened fans differs"
closewin $addr; kill $pid; wait $pid 2>/dev/null
```
- pass when: each companion prints `look-ok`; `windows after second open: 1` (never two); the printed `x=`/`y=` equal the reference's `x y` (the `shot <title> w h x y` line) within 4 px (the Hyprland rules in `rules.lua` pin them); every close prints `closed` and `service alive`; main-close prints `main close hides, service alive`; main reopens; the reopened Fans and Power window prints `reopened fans look-ok` (a closed companion is rebuilt fresh, not stuck in a half-destroyed state).
- fail when: a companion window has the wrong title or class (the rules then misplace it), opens twice, differs in size or look, a close kills the service, or closing the main window quits the app instead of hiding it.

## E9: Hostile and concurrent D-Bus input
- covers: FR-7, FR-3
- check: write and run as an `E9` private-bus script:
```
. specs/rust-rewrite/harness/lib.sh
app 2>$T/e9.err & pid=$!; wait_name $CTL_NAME 5
for bad in "OpenWindow s ''" "OpenWindow s nonsense" "OpenWindow s ../../etc/passwd" "OpenWindow s $(head -c 5000 /dev/zero | tr '\0' x)" "OpenWindow i 7" "NoSuchMethod" "OpenWindow"; do
  eval "ctl $bad" >/dev/null 2>$T/e9.one; echo "rc=$? [${bad:0:30}] $(head -c 80 $T/e9.one)"
  ping_svc 2 >/dev/null || echo "FAIL service died after: ${bad:0:30}"
done
: > $FAKE_LOG; pids=; for i in $(seq 1 20); do "$BIN" --cycle-profile & pids="$pids $!"; done; wait $pids; sleep 2
echo "profile writes logged: $(grep -c '^asusctl profile set' $FAKE_LOG)"
for i in $(seq 1 50); do ctl Open >/dev/null & done; sleep 3
ping_svc 2 >/dev/null && echo "alive after 50 Open calls" || echo "FAIL dead after Open flood"
echo "panics: $(grep -c 'panicked at' $T/e9.err)"
kill $pid; wait $pid 2>/dev/null
```
- pass when: every bad call returns a non-zero `rc` (a D-Bus error), no `FAIL service died`; `profile writes logged: 20`; `alive after 50 Open calls`; `panics: 0`.
- fail when: a malformed call is accepted or crashes the service, concurrent cycle requests are dropped or deadlock, or a flood of toggles kills it.

## E10: Polls only while visible; idle is cheap
- covers: FR-14, NFR-2
- check: write and run as an `E10` private-bus script:
```
. specs/rust-rewrite/harness/lib.sh
app 2>$T/e10.err & pid=$!; wait_name $CTL_NAME 5; sleep 4
n0=$(wc -l < $FAKE_LOG); t0=$(awk '{print $14+$15}' /proc/$pid/stat); sleep 10
n1=$(wc -l < $FAKE_LOG); t1=$(awk '{print $14+$15}' /proc/$pid/stat)
echo "hidden 10 s: tool calls=$((n1-n0)) cpu ticks=$((t1-t0)) (100/s)"
ctl Open >/dev/null; wait_win 'ROG Control' 1 3; sleep 1; n2=$(wc -l < $FAKE_LOG); sleep 6; n3=$(wc -l < $FAKE_LOG)
echo "visible 6 s: temperature polls=$(tail -n $((n3-n2)) $FAKE_LOG | grep -c 'nvidia-smi --query-gpu=temperature.gpu')"
ctl Open >/dev/null; wait_win 'ROG Control' 0 3; sleep 2; n4=$(wc -l < $FAKE_LOG); sleep 6; n5=$(wc -l < $FAKE_LOG)
echo "hidden again 6 s: tool calls=$((n5-n4))"
kill $pid; wait $pid 2>/dev/null
```
- pass when: `hidden 10 s: tool calls=0` and `cpu ticks` at most 20; `visible 6 s: temperature polls` is at least 2; `hidden again 6 s: tool calls=0`.
- fail when: it polls hardware while the window is hidden, spins the CPU at idle, never polls while visible (stale numbers), or keeps polling after the window is hidden.

## E11: Startup automation sends the same hardware commands as Python
- covers: FR-25, FR-28, FR-29
- check: write and run as an `E11` private-bus script; both implementations get the same config copy, the same fake tools and the same live UPower state:
```
. specs/rust-rewrite/harness/lib.sh
before=$(sha256sum < $H/.config/rog-control/config.json)
run() { local log=$1; shift; : > $log; FAKE_LOG=$log "$@" 2>$log.err & local p=$!
        wait_name $CTL_NAME 8 && ping_svc 8 >/dev/null; sleep 5; kill $p; wait $p 2>/dev/null; sleep 1; }
run $T/py.log   env PATH=$FAKE:$PATH HOME=$H python3 $T/ref/main.py
run $T/rust.log env PATH=$FAKE:$PATH HOME=$H "$BIN"
sort -u $T/py.log > $T/py.set; sort -u $T/rust.log > $T/rust.set
echo "--- python"; cat $T/py.set; echo "--- rust"; cat $T/rust.set; echo "--- diff"; diff $T/py.set $T/rust.set && echo identical
echo "writes: $(grep -cE '^(asusctl (profile|leds|armoury|aura|battery) |powerprofilesctl set|hyprctl eval|systemctl reboot)' $T/rust.set)"
[ "$(sha256sum < $H/.config/rog-control/config.json)" = "$before" ] && echo "config untouched" || echo "FAIL config rewritten at startup"
```
- pass when: `--- diff` is followed by `identical`; `config untouched`; no `systemctl reboot` and no `notify-send` line in either set; and `writes:` is at least 5. Measured on this machine on AC power with the config above, the Python reference's startup set is: `asusctl profile set Performance`, `powerprofilesctl set performance`, `asusctl armoury set panel_overdrive 1`, `hyprctl eval hl.monitor({output = "eDP-1", mode = "2560x1440@240", position = "0x0", scale = 1})`, `asusctl leds set high`, plus the read `hyprctl monitors -j`. On battery the profile, power plan, refresh preset and keyboard level differ, but the two sets must still be identical.
- fail when: an argument differs from Python's (e.g. `hyprctl keyword`, `asusctl leds set med` vs `medium`), a command is missing or extra, automation is applied with a notification at startup, the config is rewritten on load, or a different tool is called by absolute path (it would escape the fakes and appear nowhere in the log).

## E12: The ported test suite is complete, passes, and touches no hardware
- covers: FR-16, FR-17, FR-18, FR-19, FR-20, FR-21, FR-22, FR-23, FR-24, FR-27, FR-30, FR-38, NFR-1
- check:
```
py=$(git show 509d5e4:tests/test_e2e.py | awk '/^class /{c=$2; sub(/\(.*/,"",c)} /^    def test_/{n=$2; sub(/\(.*/,"",n); print c "." n}' | sort)
rs=$(grep -rhoE '// ports: [A-Za-z0-9_]+\.test_[A-Za-z0-9_]+' src tests 2>/dev/null | sed 's|// ports: ||' | sort)
echo "python tests: $(echo "$py" | wc -l)   ported: $(echo "$rs" | sort -u | wc -l)"; diff <(echo "$py") <(echo "$rs") && echo "names identical"
: > $FAKE_LOG; HOME=$H PATH=$FAKE:$PATH cargo test --locked 2>&1 | tail -n 15
echo "non-read tool calls during tests:"; grep -vE '^(hyprctl monitors -j|nvidia-smi --query-gpu=|nvidia-smi -q -d POWER|nvidia-settings -q |systemctl --user is-enabled rog-control.service|powerprofilesctl get|asusctl fan-curve --mod-profile [A-Za-z]+$)' $FAKE_LOG
sha256sum -c $T/realcfg.sha
grep -rn 'movewindowpixel' src | wc -l
```
- pass when: `python tests: 67   ported: 67` and `names identical`; every `test result:` line says `ok` with 0 failed and the `passed` counts sum to at least 67 (cargo prints one line per test binary; run `cargo test --locked 2>&1 | grep 'test result'` to see them all); the list under `non-read tool calls during tests:` is empty; `sha256sum -c` prints `OK` (the real config was not touched); the `movewindowpixel` count is `0`.
- fail when: a Python scenario has no Rust twin, a test is `#[ignore]`d or empty to make the count, a test shells out to a real write command (it would show in the list), the real config file changes, or the build needs a display the tests cannot get.

## E13: The tests actually catch broken rules (mutation check)
- covers: FR-17, FR-21, FR-22, FR-23, FR-27, FR-29, FR-38
- check: in a scratch copy, break one behaviour at a time, run `cargo test --locked`, then restore. The tester finds each spot by reading the code; each row names the observable behaviour to break:
```
mkdir -p $T/mut && git ls-files -z | xargs -0 tar -cf - | tar -xf - -C $T/mut && cd $T/mut
# for each row below: edit the source, run:  HOME=$H PATH=$FAKE:$PATH cargo test --locked 2>&1 | grep -E '^test .*FAILED|test result'; then undo the edit
```
  1. GPU: allow disabling the dGPU while an external display is connected through the NVIDIA card.
  2. GPU: write `dgpu_disable` in the same call as a MUX change (or write the MUX when it is not changing).
  3. GPU: perform a MUX change, or run `systemctl reboot`, without the confirmation step.
  4. NVIDIA: treat `nvidia-smi -pl` exit code 0 as success whatever its stdout says.
  5. Debounce: apply the PL1 slider directly on every value-changed instead of through the 150 ms debouncer.
  6. Fan curve: remove the clamp that keeps fan speed non-decreasing when a point is dragged.
  7. Fan curve: allow Factory Defaults while the edited profile is not the active one.
  8. Errors: swallow a failed `asusctl armoury set` (no toast).
  9. Profile cycling: swap forward and backward.
  10. Config: save without the temp-file-and-rename step, or drop unknown keys on save.
  11. Sampling: the tester picks five more ported tests from five different Python classes not touched above (e.g. screen-preset position, RGB reset-to-theme, battery slider range, startup toggle, GPU temperature flicker) and breaks the behaviour each one claims to guard.
  12. Aura: make the asusd Aura object path a constant again (bypass the runtime discovery) or make discovery pick the wrong object.
- pass when: every one of the twelve edits (row 11 counts as five) makes `cargo test` report at least one `FAILED` test (name the test in results.md) and the unedited copy passes (`cargo test` ok before each edit).
- fail when: any edit still gives `test result: ok`: the port's tests do not guard that rule, so the rule can regress silently.

## E14: Install into a clean home
- covers: FR-31, FR-32
- check: installs from a copy of the committed tree into a throwaway home, with the real `systemctl` replaced by the logging fake:
```
src=$T/src; rm -rf $src; mkdir -p $src; git ls-files -z | xargs -0 tar -cf - | tar -xf - -C $src
HI=$T/inst; rm -rf $HI; mkdir -p $HI
inst() { (cd $src && env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=$HI PATH=$FAKE:$PATH ./install.sh "$@"); }
U=$HI/.config/systemd/user/rog-control.service; D=$HI/.local/share/applications/com.orneyfish.RogControl.desktop
O=specs/rust-rewrite/harness
: > $FAKE_LOG; inst; echo "install rc=$?"
file $HI/.local/bin/rog-control | grep -c ELF
diff <(sed 's|^ExecStart=.*||' $O/orig-rog-control.service) <(sed 's|^ExecStart=.*||' $U) && echo "unit identical except ExecStart"
grep '^ExecStart=' $U; echo "expect: ExecStart=$HI/.local/bin/rog-control"
diff <(sed 's|^Exec=.*||' $O/orig-com.orneyfish.RogControl.desktop) <(sed 's|^Exec=.*||' $D) && echo "desktop identical except Exec"
grep '^Exec=' $D; echo "expect: Exec=$HI/.local/bin/rog-control"
echo "python/checkout references: $(grep -ciE 'python|main\.py|Projects/rog-control' $U $D | tr '\n' ' ')"
desktop-file-validate $D && echo "desktop valid"
find $HI/.local/share/icons -type f | sort
echo "--- systemctl calls"; grep '^systemctl' $FAKE_LOG
: > $FAKE_LOG; inst --no-enable >/dev/null; echo "with --no-enable, systemctl calls: $(grep -c '^systemctl' $FAKE_LOG)"
```
- pass when: `install rc=0`; ELF count `1`; both `identical except` lines print; the `ExecStart=` and `Exec=` lines equal their `expect:` lines; both reference counts are `0`; `desktop valid`; the icon list contains `.../hicolor/scalable/apps/com.orneyfish.RogControl.svg` and `.../hicolor/scalable/status/rog-control-symbolic.svg`; the systemctl calls are exactly, in order, `systemctl --user daemon-reload`, `systemctl --user enable rog-control.service`, `systemctl --user restart rog-control.service`; with `--no-enable` the count is `0`.
- fail when: the build or any step needs sudo or the network beyond cargo's own fetch, a unit/desktop file differs in anything but the exec path, either refers to python or the checkout, an icon is missing, or the systemctl sequence is different (restart before reload means the new binary may not run).

## E15: The installed app works with the checkout gone
- covers: FR-33
- check: continue from E14 (same `HI`, `src`, `FAKE`); move the source copy away (it keeps its build cache for E16-E18), then run the installed binary with only the installed files, as an `E15` private-bus script:
```
mv $src $T/src.gone
. specs/rust-rewrite/harness/lib.sh
python3 specs/rust-rewrite/harness/fake-watcher.py $T/reg15.txt & wpid=$!; sleep 1
env PATH=$FAKE:$PATH HOME=$HI $HI/.local/bin/rog-control 2>$T/e15.err & pid=$!
wait_name $CTL_NAME 5 && ping_svc 5 >/dev/null && echo "up" || echo "FAIL not up"
for i in $(seq 1 60); do [ -s $T/reg15.txt ] && break; sleep 0.1; done
u=$(cut -d/ -f1 $T/reg15.txt)
p=$(busctl --user get-property $u /StatusNotifierItem org.kde.StatusNotifierItem IconThemePath | sed 's/^s "\(.*\)"$/\1/')
echo "IconThemePath=$p"; test -f "$p/hicolor/scalable/status/rog-control-symbolic.svg" && echo "tray icon file found" || echo "FAIL tray icon missing"
case "$p" in $HI/*) echo "inside the install prefix";; *) echo "FAIL points outside the install";; esac
ctl Open >/dev/null; wait_win 'ROG Control' 1 3 && echo "window opens" || echo "FAIL window"
kill $pid $wpid
```
- pass when: `up`, `tray icon file found`, `inside the install prefix`, `window opens`.
- fail when: the binary or its icons are looked up in the (now deleted) checkout, the tray icon path points at the repo, or startup needs a file that only exists in the repo.

## E16: Install is idempotent and keeps rollback copies
- covers: FR-34
- check: start from a home that holds the Python-era unit and desktop entry:
```
src=$T/src; mv $T/src.gone $src          # the E14 copy again, build cache included
HI=$T/inst2; rm -rf $HI; mkdir -p $HI/.config/systemd/user $HI/.local/share/applications
O=specs/rust-rewrite/harness; U=$HI/.config/systemd/user/rog-control.service; D=$HI/.local/share/applications/com.orneyfish.RogControl.desktop
cp $O/orig-rog-control.service $U; cp $O/orig-com.orneyfish.RogControl.desktop $D
inst() { (cd $src && env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=$HI PATH=$FAKE:$PATH ./install.sh "$@"); }
snap() { (cd $HI && find . -type f | sort | xargs sha256sum); }
inst >/dev/null; snap > $T/s1; inst >/dev/null; snap > $T/s2
diff $T/s1 $T/s2 && echo "second install changed nothing"
cmp $U.pre-rust $O/orig-rog-control.service && echo "unit backup is the exact original"
cmp $D.pre-rust $O/orig-com.orneyfish.RogControl.desktop && echo "desktop backup is the exact original"
cmp -s $U $O/orig-rog-control.service && echo "FAIL live unit is still the Python one" || echo "live unit replaced"
echo "this is not the unit" > $U; inst >/dev/null
cmp $U.pre-rust $O/orig-rog-control.service && echo "backup never overwritten"
grep -c '^ExecStart=' $U
```
- pass when: all five confirmation lines print (`second install changed nothing`, both exact-original lines, `live unit replaced`, `backup never overwritten`) and the last count is `1`.
- fail when: a second run duplicates or alters files, the backup is missing, is not byte-identical to what was there, or gets clobbered by a later run (rollback then restores garbage).

## E17: Failed build and missing prerequisites change nothing
- covers: FR-31, FR-34
- check: reuse `HI`, `src`, `inst` from E16 (a fully installed home), snapshot it, then break things:
```
snap > $T/before
mkdir -p $T/badcargo; printf '#!/bin/sh\necho "cargo: simulated failure" >&2\nexit 1\n' > $T/badcargo/cargo; chmod +x $T/badcargo/cargo
: > $FAKE_LOG; (cd $src && env HOME=$HI PATH=$T/badcargo:$FAKE:$PATH ./install.sh); echo "failed-build rc=$?"
snap > $T/after; diff $T/before $T/after && echo "failed build: nothing changed"
echo "systemctl state changes: $(grep -cE '^systemctl .*(stop|restart|disable|enable|daemon-reload)' $FAKE_LOG)"
# no cargo at all
mkdir -p $T/nocargo; for f in /usr/bin/*; do b=${f##*/}; case $b in cargo|cargo-*|rustc|rustup) ;; *) ln -sf $f $T/nocargo/$b;; esac; done
(cd $src && env HOME=$HI PATH=$FAKE:$T/nocargo ./install.sh) >$T/pre1.out 2>&1; echo "no-cargo rc=$?"; head -5 $T/pre1.out
# no GTK/libadwaita development files
(cd $src && env HOME=$HI PATH=$FAKE:$PATH PKG_CONFIG_PATH=/nonexistent PKG_CONFIG_LIBDIR=/nonexistent ./install.sh) >$T/pre2.out 2>&1; echo "no-gtk rc=$?"; head -5 $T/pre2.out
snap > $T/after2; diff $T/before $T/after2 && echo "prerequisite failures: nothing changed"
echo "systemctl state changes: $(grep -cE '^systemctl .*(stop|restart|disable|enable|daemon-reload)' $FAKE_LOG)"
```
- pass when: all three `rc=` values are non-zero; both `nothing changed` lines print; both `systemctl state changes` counts are `0`; the no-cargo output names `cargo` and contains `sudo pacman -S rust gtk4 libadwaita`; the no-gtk output names `gtk4` and `libadwaita`.
- fail when: a failed build leaves a half-installed or replaced file, stops or restarts the running service anyway, or a missing prerequisite produces a raw cargo/pkg-config stack instead of a clear message.

## E18: Uninstall removes only what was installed
- covers: FR-35
- check: reuse `HI`, `src`, `inst` from E16:
```
mkdir -p $HI/.config/rog-control; echo '{"automation":{"keep":1}}' > $HI/.config/rog-control/config.json
echo x > $HI/.local/bin/other-tool; echo x > $HI/.config/systemd/user/other.service
keep() { sha256sum $HI/.config/rog-control/config.json $HI/.local/bin/other-tool $HI/.config/systemd/user/other.service; }
keep > $T/keep1; : > $FAKE_LOG; inst --uninstall; echo "uninstall rc=$?"
echo "--- systemctl"; grep '^systemctl' $FAKE_LOG
echo "--- left behind"; find $HI \( -name 'rog-control*' -o -name 'com.orneyfish.RogControl*' \) -type f | sort
keep > $T/keep2; diff $T/keep1 $T/keep2 && echo "user data and unrelated files untouched"
inst --uninstall; echo "second uninstall rc=$?"
```
- pass when: `uninstall rc=0`; the systemctl lines are `systemctl --user disable --now rog-control.service` followed by `systemctl --user daemon-reload`; the "left behind" list holds only `*.pre-rust` backups and the config file (no binary, unit, desktop entry or icon); `user data and unrelated files untouched`; `second uninstall rc=0`.
- fail when: it deletes the user's config or any unrelated file, leaves the binary/unit/icons, doesn't stop the service, or errors when run twice.

## E19: Keybinds are edited surgically and still load
- covers: FR-36
- check:
```
KB=$HOME/.config/hypr/custom/keybinds.lua; OLD=8d71f8ddc9b97f9fa84f1c51e732c70a43dbde191524babd6110951d89ea7780
sha256sum $KB.pre-rust | grep -c $OLD                                  # backup is the untouched original
sed 's#/home/orneyfish/.local/bin/rog-control#/usr/bin/python3 /home/orneyfish/Projects/rog-control/main.py#g' $KB | sha256sum | grep -c $OLD   # reversing the path edit reproduces the original byte for byte
wc -l < $KB; grep -c 'main.py' $KB; grep -n 'rog-control' $KB
for k in 'XF86Launch1' 'CTRL + SHIFT + F12' 'CTRL + SHIFT + F5' 'CTRL + SHIFT + ALT + F5'; do grep -F "\"$k\"" $KB | sed 's/.*exec_cmd("\([^"]*\)").*/\1/'; done
test -x /home/orneyfish/.local/bin/rog-control && echo "bound binary exists"
luac -p $KB && echo "lua parses"
hyprctl reload; sleep 2; echo "config errors: [$(hyprctl configerrors)]"
```
- pass when: the two `grep -c` hash checks print `1` and `1`; line count is `108`; `main.py` count `0`; exactly four `rog-control` lines; the four extracted commands are exactly `/home/orneyfish/.local/bin/rog-control --open`, `... --open`, `... --cycle-profile`, `... --cycle-profile-back`; `bound binary exists`; `lua parses`; `config errors: []`.
- fail when: any other line of the file changed (the reverse-edit hash differs), a bind still runs Python, a flag is wrong (the ROG key would cycle instead of open), the backup is missing, or Hyprland reports a config error after reload.

## E20: Hardware state is unchanged by everything above
- covers: NFR-1
- check: run it AFTER E21 has finished, not before (E19's real `hyprctl reload` re-applies the compositor's own monitor rule, `mode = "preferred"`, which drops the panel to 60 Hz until the service's startup automation sets 240 Hz again; E21 starts that service). Wait for the automation first: `sleep 5; specs/rust-rewrite/harness/hw-snapshot.sh > $T/hw.after; diff $T/hw.before $T/hw.after && echo hardware-unchanged; sha256sum -c $T/realcfg.sha`
- pass when: the diff is empty and `hardware-unchanged` prints (profile, GPU MUX, dGPU, overdrive, battery limit, keyboard level, fan curves, power plan, refresh rate all as before), and the real config hash still matches.
- fail when: anything differs: an eval or test reached real hardware despite the fakes, so a code path bypasses `PATH` lookup or a test is not mocked.

## E21: Real deployment on this machine
- covers: FR-36, FR-37
- check: this is the machine's real service, session bus and `$HOME`. Do not run the cycle binds (they would change the real profile); the `--open` binds are safe.
```
. specs/rust-rewrite/harness/lib.sh
systemctl --user start rog-control.service; sleep 3
systemctl --user is-active rog-control.service
systemctl --user show -p ExecStart --value rog-control.service | grep -o '/home/orneyfish/.local/bin/rog-control' | head -1
echo "python processes: $(pgrep -fc 'python.*main\.py')   rog-control processes: $(pgrep -xc rog-control)"
KB=$HOME/.config/hypr/custom/keybinds.lua
for k in 'XF86Launch1' 'CTRL + SHIFT + F12'; do
  cmd=$(grep -F "\"$k\"" $KB | sed 's/.*exec_cmd("\([^"]*\)").*/\1/')
  $cmd; wait_win 'ROG Control' 1 3 && echo "$k: opens" || echo "FAIL $k did not open"
  $cmd; wait_win 'ROG Control' 0 3 && echo "$k: closes" || echo "FAIL $k did not close"
done
gtk-launch com.orneyfish.RogControl; wait_win 'ROG Control' 1 3 && echo "launcher entry: opens" || echo "FAIL launcher"; gtk-launch com.orneyfish.RogControl; wait_win 'ROG Control' 0 3
echo "panics in journal: $(journalctl --user -u rog-control.service --since '-10min' --no-pager | grep -c 'panicked at')"
```
- pass when: `active`; ExecStart is the Rust binary; `python processes: 0   rog-control processes: 1`; both binds and the launcher entry open and close the window; no panics in the journal.
- fail when: the service is inactive, still runs Python, a second process is left by the launcher entry, or a hotkey does nothing.

## E22: Docs updated, Python removed, history intact
- covers: FR-39, FR-40
- check:
```
awk '/^#+ .*[Rr]ollback/{r=1;next} /^#+ /{r=0} /python|main\.py|unittest/{ if(!r) print NR": "$0}' CLAUDE.md   # outside the rollback section
grep -c 'cargo test' CLAUDE.md; grep -c 'install.sh' CLAUDE.md; grep -c 'pre-rust' CLAUDE.md; grep -c '509d5e4' CLAUDE.md
git ls-files '*.py' | grep -v '^specs/'; echo "py outside specs: $(git ls-files '*.py' | grep -vc '^specs/')"
git cat-file -e 509d5e4:main.py && echo "history intact"
git check-ignore target && echo "target ignored"; git branch --show-current; git status --porcelain | grep -vcE '^\?\? \.claude/|^.. specs/rust-rewrite/(evidence/|results\.md)'
```
- pass when: the awk prints nothing; the four greps are each at least `1`; `py outside specs: 0`; `history intact`; `target ignored`; branch `feat/rust-rewrite`; the last count is `0` (everything committed; the untracked `.claude/` directory is not part of this work, and neither are this test run's own `specs/rust-rewrite/evidence/` and `results.md`, which the run rewrites).
- fail when: CLAUDE.md still tells the next reader to run `python3 main.py` or `unittest`, the rollback recipe is missing, Python files remain, or build output is not ignored.

## Definition of done
- [ ] Every eval above has a pass row in results.md, with evidence (the command output, and the two PNG paths for E7/E8)
- [ ] results.md holds the 67-row Python-to-Rust test mapping table and, for E13, the failing test name for each of the sixteen mutations (eleven named, five sampled)
- [ ] Nothing under "Out of scope" was built and nothing under "Must not touch" changed (`rules.lua` hash is still `0004b7328141ad28049a6b2334647a4cfbd0dad909b7fbb8b02ddf9f6ccfec1f`; `specs/rust-rewrite/{RD.md,evals.md,mockup.html,ref,harness}` untouched)
- [ ] The project's lint and build pass: `cargo build --release --locked`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`
- [ ] The real `rog-control.service` is running the Rust binary at the end (E21), and `git worktree remove --force $T/ref` has cleaned up the reference checkout
