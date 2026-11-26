//! The two host waves of a console wave meeting after a loop its lanes leave in different
//! rounds: what the wave counts, elects and numbers there is over all 64 lanes.

use super::paired::{EXEC, LIT, VCC, k, run_paired, sop1, sop2, sopp, store, stores, v, vop1, vop2, vop3, vopc};
use super::*;

/// One console wave of 64 threads. Lane t goes round a loop `(t & s0) + 1` times, adding
/// `t & s1` to its sum each round; the loop is the compiler's usual one for lanes that leave
/// apart (a mask of the lanes still going round, the wave's branch back while some lane is
/// left). After it, over the lanes whose `t & s1` is not 0: their count (`s_bcnt1_i32_b64`),
/// the first of them (`v_readfirstlane_b32` of the thread index) and each one's number among
/// them (`v_mbcnt_lo/hi`). Every thread stores its sum, its number (0x55 where it has none),
/// the count and the first.
fn program() -> NativeProgram {
    let mut w = vec![
        vop2(0x1B, 4, 0, 0), // v_and_b32 v4, s0, v0: the lane's last round
        vop2(0x1B, 5, 1, 0), // v_and_b32 v5, s1, v0: what the lane adds a round
        vop1(0x01, 7, k(0)), // v_mov_b32 v7, 0
        vop1(0x01, 8, LIT),  // v_mov_b32 v8, 0x55
        0x55,
        sop1(0x03, 6, k(0)),      // s_mov_b32 s6, 0: the round
        sop1(0x04, 2, EXEC),      // s_mov_b64 s[2:3], exec
        sop1(0x04, 12, EXEC),     // s_mov_b64 s[12:13], exec: the lanes still going round
        sop1(0x04, 14, EXEC),     // s_mov_b64 s[14:15], exec
        sop2(0x0F, EXEC, 14, 12), // loop: s_and_b64 exec, s[14:15], s[12:13]
        vop2(0x25, 7, v(5), 7),   // v_add_nc_u32 v7, v5, v7
        sop1(0x04, EXEC, 14),     // s_mov_b64 exec, s[14:15]
        sop2(0x02, 6, 6, k(1)),   // s_add_i32 s6, s6, 1
    ];
    w.extend(vop3(0xC4, 14, 6, v(4))); // v_cmp_gt_u32 s[14:15], s6, v4: the lanes that are done
    w.extend([
        sop1(0x24, 14, 14),       // s_and_saveexec_b64 s[14:15], s[14:15]
        sop2(0x15, 12, 12, EXEC), // s_andn2_b64 s[12:13], s[12:13], exec
        sopp(0x05, -9),           // s_cbranch_scc1 loop
        sop1(0x04, EXEC, 2),      // s_mov_b64 exec, s[2:3]
        vopc(0xD5, k(0), 5),      // v_cmpx_ne_u32 exec, 0, v5
        sop1(0x10, 7, EXEC),      // s_bcnt1_i32_b64 s7, exec
        vop1(0x02, 16, v(0)),     // v_readfirstlane_b32 s16, v0
    ]);
    w.extend(vop3(0x365, 8, EXEC, k(0))); // v_mbcnt_lo_u32_b32 v8, exec_lo, 0
    w.extend(vop3(0x366, 8, EXEC + 1, v(8))); // v_mbcnt_hi_u32_b32 v8, exec_hi, v8
    w.extend([
        sop1(0x04, EXEC, 2), // s_mov_b64 exec, s[2:3]
        vop1(0x01, 9, 7),    // v_mov_b32 v9, s7
        vop1(0x01, 10, 16),  // v_mov_b32 v10, s16
    ]);
    w.extend(stores([7, 8, 9, 10]));
    native_with(&w, true, 64, 0)
}

/// What the console's 64 lanes store with the root constants `s0` and `s1`.
fn console(s0: u32, s1: u32) -> Vec<u32> {
    let counted: Vec<u32> = (0..64).filter(|t| t & s1 != 0).collect();
    (0..64u32)
        .flat_map(|t| {
            let number = if t & s1 != 0 {
                counted.iter().filter(|&&u| u < t).count() as u32
            } else {
                0x55
            };
            [(t & s1) * ((t & s0) + 1), number, counted.len() as u32, counted[0]]
        })
        .collect()
}

#[test]
fn lanes_that_leave_a_loop_apart_are_counted_together_after_it() {
    let native = program();
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(&p, &native).expect("io::raise");
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes).expect("container");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let made = emit_forms(&c, &native, &p, &st, &io, &res);
    lower::TEST_HOST_WAVE.set(was);
    let (shader, wave) = made.expect("a form for hosts of 32-lane waves");
    assert_eq!(wave, Some(32));
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let _one = valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // (s0, s1): every lane one round; then lanes of one host wave leaving in different rounds,
    // counted lanes in both halves, in the upper half only, and in the lower half only
    for (s0, s1) in [(0u32, 1u32), (3, 1), (7, 5), (24, 63), (7, 32), (7, 16), (63, 9)] {
        let Some(got) = run_paired(&shader, [s0, s1, 0, 0], 256) else { return };
        let want = console(s0, s1);
        if let Some(n) = (0..want.len()).find(|&n| got[n] != want[n]) {
            let wrong = (0..64).filter(|t| got[4 * t..4 * t + 4] != want[4 * t..4 * t + 4]).count();
            panic!(
                "s0 {s0}, s1 {s1}: thread {}, dword {}: {:#x}, the console's {:#x} ({wrong} of 64 threads differ)",
                n / 4,
                n % 4,
                got[n],
                want[n]
            );
        }
    }
    eprintln!("ran on the hardware device: counts, first lanes and numbers after a loop as on the console");
}

/// The lanes of one class numbered into a list: the first of them adds their count to the
/// counter at `counter` (`buffer_atomic_add`), every one takes what the counter held before
/// (`v_readfirstlane_b32`) plus its number among them (`v_mbcnt_lo/hi`) into v11.
fn numbered(counter: u32) -> Vec<u32> {
    let mut w = Vec::new();
    w.extend(vop3(0x365, 11, 193, k(0))); // v_mbcnt_lo_u32_b32 v11, -1, 0
    w.extend([
        sop1(0x04, 16, EXEC), // s_mov_b64 s[16:17], exec
        sop1(0x14, 18, EXEC), // s_ff1_i32_b64 s18, exec
        sop1(0x04, 20, EXEC), // s_mov_b64 s[20:21], exec
    ]);
    w.extend(vop3(0x366, 12, 193, v(11))); // v_mbcnt_hi_u32_b32 v12, -1, v11: the lane's index
    w.extend([
        vop1(0x01, 11, k(0)),  // v_mov_b32 v11, 0
        vopc(0x92, 18, 12),    // v_cmpx_eq_i32 exec, s18, v12: the first lane alone
        sopp(0x08, 4),         // s_cbranch_execz past the addition
        sop1(0x10, 22, 16),    // s_bcnt1_i32_b64 s22, s[16:17]
        vop1(0x01, 11, 22),    // v_mov_b32 v11, s22
        0xE0C8_4000 | counter, // buffer_atomic_add v11, s[8:11], 0 offset:<counter> glc
        0x8002_0000 | 11 << 8,
        sop1(0x04, EXEC, 20), // s_mov_b64 exec, s[20:21]
    ]);
    w.extend(vop3(0x365, 12, EXEC, k(0))); // v_mbcnt_lo_u32_b32 v12, exec_lo, 0
    w.push(vop1(0x02, 22, v(11))); // v_readfirstlane_b32 s22, v11
    w.extend(vop3(0x366, 11, EXEC + 1, v(12))); // v_mbcnt_hi_u32_b32 v11, exec_hi, v12
    w.push(vop2(0x25, 11, 22, 11)); // v_add_nc_u32 v11, s22, v11
    w
}

/// Two console waves of 64 threads filling two lists the way a compiler's tile classification
/// does. A thread takes part when `(t + 1) & s2` is not 0. It goes round a loop at most
/// `(t & 7) + s0 + 1` times (a number of rounds no compiler can bound), adding `t & s1` to its
/// sum each round, and leaves early in the round its sum reaches `s3`; the loop has skips
/// inside and is entered past its first instruction.
/// After it the threads that left early are one class and the others another; each class
/// counts its lanes into its own counter (dwords 0 and 1) and numbers them ([`numbered`]),
/// and every thread stores `t + 1` in a list of 128 dwords from dword 16: the one class from
/// the list's start on, the other from its end back. Each thread also stores its class and
/// its place from dword 256 on.
fn lists() -> NativeProgram {
    let body = {
        let mut w = vec![
            sop2(0x0F, EXEC, 14, 12), // s_and_b64 exec, s[14:15], s[12:13]
            vop1(0x01, 10, k(0)),     // v_mov_b32 v10, 0 (entered here)
            sop1(0x04, 14, EXEC),     // s_mov_b64 s[14:15], exec
            vopc(0xD5, k(0), 5),      // v_cmpx_ne_u32 exec, 0, v5
            sopp(0x08, 4),            // s_cbranch_execz to the round's end
            vop2(0x25, 7, v(5), 7),   // v_add_nc_u32 v7, v5, v7
            vopc(0xD3, 3, 7),         // v_cmpx_le_u32 exec, s3, v7
            sopp(0x08, 1),            // s_cbranch_execz to the round's end
            vop1(0x01, 10, k(1)),     // v_mov_b32 v10, 1
            sop1(0x04, EXEC, 14),     // the round's end: s_mov_b64 exec, s[14:15]
            sop2(0x02, 6, 6, k(1)),   // s_add_i32 s6, s6, 1
            vopc(0xC5, k(0), 10),     // v_cmp_ne_u32 vcc, 0, v10
        ];
        w.extend(vop3(0xC4, 14, 6, v(4))); // v_cmp_gt_u32 s[14:15], s6, v4
        w.extend([
            sop2(0x11, 14, VCC, 14),  // s_or_b64 s[14:15], vcc, s[14:15]
            sop1(0x24, 14, 14),       // s_and_saveexec_b64 s[14:15], s[14:15]
            sop2(0x15, 12, 12, EXEC), // s_andn2_b64 s[12:13], s[12:13], exec
        ]);
        let back = -(w.len() as i16 + 1);
        w.push(sopp(0x05, back)); // s_cbranch_scc1 to the loop's first instruction
        w
    };
    let (first, second) = (numbered(0), numbered(4));
    let mut after = vec![
        sop1(0x04, EXEC, 24),             // s_mov_b64 exec, s[24:25]
        sop1(0x04, 14, EXEC),             // s_mov_b64 s[14:15], exec
        vopc(0xD2, k(0), 10),             // v_cmpx_eq_u32 exec, 0, v10
        sopp(0x08, first.len() as i16),   // s_cbranch_execz past the first class
    ];
    after.extend(first);
    after.extend([
        sop2(0x15, EXEC, 14, EXEC),          // s_andn2_b64 exec, s[14:15], exec
        sopp(0x08, second.len() as i16 + 2), // s_cbranch_execz past the second class
    ]);
    after.extend(second);
    after.extend([
        vop2(0x26, 11, LIT, 11), // v_sub_nc_u32 v11, 127, v11: from the list's end back
        127,
        sop1(0x04, EXEC, 14),    // s_mov_b64 exec, s[14:15]
        vop2(0x25, 13, k(1), 0), // v_add_nc_u32 v13, 1, v0
        vop2(0x1A, 14, k(2), 11), // v_lshlrev_b32 v14, 2, v11
    ]);
    after.extend(store(13, 14, 64));
    after.push(vop2(0x1A, 15, k(3), 0)); // v_lshlrev_b32 v15, 3, v0
    after.extend(store(10, 15, 1024));
    after.extend(store(11, 15, 1028));
    let mut w = vec![
        vop2(0x1B, 4, k(7), 0), // v_and_b32 v4, 7, v0
        vop2(0x25, 4, 0, 4),    // v_add_nc_u32 v4, s0, v4
        vop2(0x1B, 5, 1, 0),    // v_and_b32 v5, s1, v0
        vop2(0x25, 6, k(1), 0), // v_add_nc_u32 v6, 1, v0
        vop2(0x1B, 6, 2, 6),    // v_and_b32 v6, s2, v6
        vop1(0x01, 7, k(0)),    // v_mov_b32 v7, 0
        vopc(0xD5, k(0), 6),    // v_cmpx_ne_u32 exec, 0, v6
        sopp(0x08, (4 + body.len() + after.len()) as i16), // s_cbranch_execz to the end
        sop1(0x04, 24, EXEC),   // s_mov_b64 s[24:25], exec
        sop1(0x04, 12, EXEC),   // s_mov_b64 s[12:13], exec
        sop1(0x03, 6, k(0)),    // s_mov_b32 s6, 0
        sopp(0x02, 1),          // s_branch past the loop's first instruction
    ];
    w.extend(body);
    w.extend(after);
    native_with(&w, true, 128, 0)
}

/// The `t + 1` of the threads of [`lists`] that do not leave its loop early, and of those
/// that do.
fn classes(s: [u32; 4]) -> (Vec<u32>, Vec<u32>) {
    let (mut stay, mut early) = (Vec::new(), Vec::new());
    for t in (0..128u32).filter(|t| (t + 1) & s[2] != 0) {
        let (mut sum, mut round) = (0u32, 0u32);
        let left_early = loop {
            sum += t & s[1];
            let reached = t & s[1] != 0 && s[3] <= sum;
            round += 1;
            if reached || round > (t & 7) + s[0] {
                break reached;
            }
        };
        if left_early { early.push(t + 1) } else { stay.push(t + 1) }
    }
    (stay, early)
}

#[test]
fn lanes_that_leave_a_loop_apart_fill_their_lists_after_it() {
    let native = lists();
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(&p, &native).expect("io::raise");
    let bytes = crate::dxbc::build(&[]);
    let c = Container::parse(&bytes).expect("container");
    let was = lower::TEST_HOST_WAVE.replace(Some(32));
    let made = emit_forms(&c, &native, &p, &st, &io, &res);
    lower::TEST_HOST_WAVE.set(was);
    let (shader, wave) = made.expect("a form for hosts of 32-lane waves");
    assert_eq!(wave, Some(32));
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    let _one = valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // (s0, s1, s2, s3): no thread leaves early; threads of one host wave leaving in different
    // rounds, some early; the same with threads that take no part; more rounds; alternate
    // threads early in the first round; more rounds than a translated loop is let run (the
    // threads taken out of it at the limit fill their lists like the others), and the same
    // with threads that take no part
    let sets = [
        [0u32, 0, !0, 1],
        [0, 24, !0, 40],
        [0, 24, 3, 40],
        [3, 5, !0, 9],
        [1, 127, 5, 60],
        [0, 1, !0, 0],
        [5000, 0, !0, 1],
        [5000, 0, 5, 1],
    ];
    for s in sets {
        let Some(got) = run_paired(&shader, s, 512) else { return };
        let (stay, early) = classes(s);
        let list = &got[16..144];
        let mut front = list[..(got[0] as usize).min(128)].to_vec();
        let mut back = list[128 - (got[1] as usize).min(128)..].to_vec();
        front.sort_unstable();
        back.sort_unstable();
        let between = list[front.len()..128 - back.len().min(128 - front.len())].iter().filter(|&&e| e != 0).count();
        if (got[0], got[1]) != (stay.len() as u32, early.len() as u32) || front != stay || back != early || between != 0 {
            let lost = |want: &[u32], have: &[u32]| want.iter().filter(|t| !have.contains(t)).count();
            let places: Vec<String> = (0..128)
                .filter(|t| (t + 1) & s[2] as usize != 0)
                .map(|t| format!("{t}:{}@{}", got[256 + 2 * t], got[257 + 2 * t]))
                .collect();
            panic!(
                "{s:?}: counters {} and {}, the console's {} and {}; {} of the first list's threads and {} of the \
                 second's are not in them, {between} entries between the lists; thread:class@place {}",
                got[0],
                got[1],
                stay.len(),
                early.len(),
                lost(&stay, &front),
                lost(&early, &back),
                places.join(" ")
            );
        }
    }
    eprintln!("ran on the hardware device: two lists filled after a loop as on the console");
}
