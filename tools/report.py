# report.py <log> [report file]: the report of a session started by play.ps1.
# Frame rate, regularity, hitches, submitting thread, shaders and pipelines, card and driver,
# memory, most frequent warnings. ASCII only.
import collections
import re
import statistics
import sys


def log_lines(path):
    raw = open(path, 'rb').read()
    if raw[:2] in (b'\xff\xfe', b'\xfe\xff'):
        text = raw.decode('utf-16', errors='ignore')
    else:
        text = raw.decode('utf-8', errors='ignore')
    header, lines, current = [], [], []
    for l in text.splitlines():
        if l.startswith('# '):
            header.append(l[2:])
            continue
        if re.match(r'\d{4}-\d\d-\d\dT', l):
            if current:
                lines.append(' '.join(current))
            current = [l]
        elif current:
            current.append(l.strip())
    if current:
        lines.append(' '.join(current))
    return header, [re.sub(r'\s+', ' ', l) for l in lines], text


def clock(l):
    return l[11:19]


def median(v, default=float('nan')):
    return statistics.median(v) if v else default


def report(path):
    out = []
    say = out.append
    header, lines, raw = log_lines(path)
    say('=== SESSION REPORT ===')
    for h in header[:4]:
        say('  ' + h)
    settings = [h[len('setting '):] for h in header if h.startswith('setting ')]
    if settings:
        say('  %d settings: %s' % (len(settings), ', '.join(s.replace('VICTORY_', '') for s in settings)))
    if lines:
        say('  log from %s to %s (UTC), %d lines' % (clock(lines[0]), clock(lines[-1]), len(lines)))

    # Frames
    fps = []
    for l in lines:
        m = re.search(r'([0-9.]+) fps, ([0-9.]+) ms/frame \(([0-9.]+) preparing.*?(\d+) draws/frame, dropped (\d+) without a host pipeline \((\d+) being made\)', l)
        if m:
            fps.append((clock(l), float(m.group(1)), float(m.group(2)), float(m.group(3)), int(m.group(4)), int(m.group(5)), int(m.group(6))))
    world = [f for f in fps if f[4] >= 2500]
    say('')
    say('-- Frames (sampled every 5 s)')
    if world:
        say('  in the world: %.1f fps on average, median %.1f ms a frame (best sample %.1f, worst %.1f at %s)'
            % (statistics.mean(f[1] for f in world), median([f[2] for f in world]), min(f[2] for f in world),
               max(f[2] for f in world), max(world, key=lambda f: f[2])[0]))
        say('  draws a frame: median %d; dropped for lack of a pipeline: median %d, at most %d'
            % (median([f[4] for f in world]), median([f[5] for f in world]), max(f[5] for f in world)))
    else:
        say('  not enough samples in the world (session too short, or it stayed in the menus)')
    gaps = []
    for l in lines:
        m = re.search(r'times between frames: (\d+) frames, shortest ([0-9.]+) ms, median ([0-9.]+), p90 ([0-9.]+), longest ([0-9.]+) \(mean ([0-9.]+)\); (\d+) over 50 ms, (\d+) over 100 ms', l)
        if m:
            gaps.append((clock(l),) + tuple(float(x) for x in m.groups()))
    if gaps:
        n = sum(g[1] for g in gaps)
        say('  regularity over %d frames: median %.1f ms, 90th percentile %.1f ms; %d frames over 50 ms, %d over 100 ms'
            % (n, median([g[3] for g in gaps]), median([g[4] for g in gaps]), sum(g[7] for g in gaps), sum(g[8] for g in gaps)))
        worst = sorted(gaps, key=lambda g: -g[5])[:5]
        say('  biggest hitches: ' + ', '.join('%s %.0f ms' % (g[0], g[5]) for g in worst))

    # Submitting thread
    q = []
    for l in lines:
        m = re.search(r'per frame \(VICTORY_ASYNC_SUBMIT\): ([0-9.]+) in ([0-9.]+) presents.*?that thread worked ([0-9.]+) ms; threads of the title waited ([0-9.]+) ms for room in the line, ([0-9.]+) ms at presents', l)
        if m and float(m.group(1)) > 10:
            q.append(tuple(float(x) for x in m.groups()))
    if q:
        say('')
        say('-- Submitting thread (victory-submit)')
        say('  work: median %.1f ms a frame; the title waits %.1f ms at present'
            % (median([x[2] for x in q]), median([x[4] for x in q])))

    # Shaders and pipelines
    say('')
    say('-- Shaders and pipelines')
    sc = [re.search(r'hits=(\d+) misses=(\d+)', l) for l in lines if 'shaders from the cache and translated so far' in l]
    sc = [m for m in sc if m]
    if sc:
        say('  shaders: %s from the cache, %s translated during the session' % (sc[-1].group(1), sc[-1].group(2)))
    made = sum(1 for l in lines if ' host pipeline shaders=' in l or re.search(r'INFO victory_gpu::pipeline::host: .*host pipeline', l))
    refused = [l for l in lines if 'pipeline refused' in l or 'root signature refused' in l]
    say('  pipelines made: %d; refused by the driver: %d' % (made, len(refused)))
    for l in refused[:4]:
        say('    REFUSED %s %s' % (clock(l), l[28:240]))
    missing = [l for l in lines if 'pipeline creation method that is not implemented' in l]
    if missing:
        say('  pipeline creation methods Victory does not have (the title goes without): %d descriptions' % len(missing))

    # Card and driver
    say('')
    say('-- Card and driver')
    errors = [l for l in lines if ' ERROR ' in l]
    lost = [l for l in lines if 'device removed' in l]
    say('  errors: %d; card losses: %d' % (len(errors), len(lost)))
    for l in errors[:3]:
        say('    %s %s' % (clock(l), l[28:300]))
    if 'unhandled exception' in raw or 'panicked' in raw:
        say('  CRASH recorded in the log')

    # Memory
    vm = [re.search(r"card's own: budget (\d+) MB, in use (\d+) MB.*?shared with the card: budget (\d+) MB, in use (\d+) MB", l) for l in lines if 'video memory as the system counts it' in l]
    vm = [m for m in vm if m]
    if vm:
        m = vm[-1]
        say('')
        say('-- Memory')
        say("  card: %s MB used of %s; PC memory shared with the card: %s MB of %s" % (m.group(2), m.group(1), m.group(4), m.group(3)))

    # Most frequent warnings
    c = collections.Counter()
    for l in lines:
        m = re.search(r' (WARN|ERROR) (\S+): (.{0,70})', l)
        if m:
            c[m.group(1) + ' ' + m.group(2).replace('victory_gpu::', '') + ': ' + re.sub(r'0x[0-9a-f]+|\d+', '#', m.group(3))] += 1
    if c:
        say('')
        say('-- Most frequent warnings')
        for k, v in c.most_common(8):
            say('  %5d  %s' % (v, k))
    return out


if __name__ == '__main__':
    text = report(sys.argv[1])
    print('\n'.join(text))
    if len(sys.argv) > 2:
        with open(sys.argv[2], 'w', encoding='utf-8') as f:
            f.write('\n'.join(text) + '\n')
