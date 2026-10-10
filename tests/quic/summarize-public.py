#!/usr/bin/env python3
"""Summarize recorded public probes; no network calls and no invented samples."""
import json
import re
import statistics
import sys
from pathlib import Path


def median(values):
    return statistics.median(values) if values else None


def union_length(intervals):
    end, total = 0, 0
    for start, stop in sorted(intervals):
        total += max(0, stop - max(start, end))
        end = max(end, stop)
    return total


def stalls(events, until):
    waits = [e for e in events if e['name'] == 'waiting' and e['phase'] in ('play', 'offline', 'recovery')]
    intervals = []
    for event in waits:
        stop = next((e['milliseconds'] for e in events if e['milliseconds'] > event['milliseconds']
                     and e['name'] in ('playing', 'pause', 'seeking', 'ended')), until)
        intervals.append((event['milliseconds'], stop))
    return len(waits), union_length(intervals)


def normalize_media(sample):
    requests = sample['requests']
    # Teardown events in an earlier harness could update a request after its
    # aggregate was taken. The saved per-request observations are authoritative.
    sample['network']['requestCount'] = len(requests)
    sample['network']['receivedBodyBytes'] = sum(r['bodyBytes'] for r in requests)
    sample['network']['failedRequests'] = sum(bool(r.get('failed')) for r in requests)
    sample['network']['nonCancelledFailures'] = sum(bool(r.get('failed')) and not r.get('cancelled', False) for r in requests)
    sample['network']['protocols'] = sorted({r['protocol'] for r in requests if r.get('protocol')})
    # Older interrupted harness samples retain request details, but lack media
    # events. Report those counts as unmeasured rather than zero stalls.
    if sample.get('media'):
        count, duration = stalls(sample['media']['events'], sample['media']['totalWallMs'])
    else:
        count, duration = None, None
    sample['network']['playbackWaitingEvents'] = count
    sample['network']['playbackStallMs'] = duration
    intervals, received = [], 0
    for request in sample['requests']:
        match = re.fullmatch(r'bytes (\d+)-(\d+)/(\d+)', request.get('contentRange') or '')
        if not match or not request['bodyBytes']:
            continue
        start, stop, _ = map(int, match.groups())
        if request['bodyBytes'] > stop - start + 1:
            raise ValueError('Observed bytes exceed the declared response range')
        intervals.append((start, start + request['bodyBytes']))
        received += request['bodyBytes']
    unique = union_length(intervals)
    sample['network']['uniqueObservedPayloadBytes'] = unique
    sample['network']['overlappingObservedPayloadBytes'] = received - unique
    sample['network']['payloadOverlapMethod'] = 'Union of received response prefixes at Content-Range offsets; excludes QUIC/TCP retransmissions.'
    if sample.get('initialPlayback'):
        initial = sample['initialPlayback']
        initial['waitingEvents'], initial['stallMs'] = stalls(initial['events'], initial['elapsedMs'])
    if sample.get('interruption'):
        sample['interruption']['networkInterruptionObserved'] = any(r.get('failed') == 'net::ERR_INTERNET_DISCONNECTED' for r in sample['requests'])
        if not sample['interruption']['networkInterruptionObserved']:
            sample['interruption']['limitation'] = 'Offline commands were applied, but no disconnected request proves interruption of the active QUIC stream; fault recovery is inconclusive.'


def main():
    folder = Path(sys.argv[1])
    network = json.loads((folder / 'network-after-fix.json').read_text())
    report = {'network': {}, 'media': {}, 'diagnostics': {}, 'unmeasured': [
        'Chinese residential/mobile ISP coverage', 'Multiple evenings and off-peak comparison',
        'Real packet loss per hop, mobile network handover', '4K or bitrate above the 14.31 Mbps available fixture',
        'Complete Revaro player UI, hardware-specific visual quality, and other browser platforms',
        'Alternate QUIC congestion-control modes under identical conditions']}
    bounded_hashes = {s['sha256'] for s in network['boundedRanges'] if s['rangeContractVerified']}
    random_hashes = {}
    for s in network['randomRanges']:
        random_hashes.setdefault(s['range'], set()).add(s['sha256'])
    report['rangeValidation'] = {'boundedSamples': len(network['boundedRanges']), 'randomSamples': len(network['randomRanges']),
        'allContractsVerified': all(s['protocolVerified'] and s['rangeContractVerified'] for s in network['boundedRanges'] + network['randomRanges']),
        'boundedCrossProtocolHashesMatch': len(bounded_hashes) == 1,
        'randomCrossProtocolHashesMatch': all(len(hashes) == 1 for hashes in random_hashes.values())}
    for protocol in ('h2', 'h3'):
        random = [s for s in network['randomRanges'] if s['requestedProtocol'] == protocol]
        report['network'][protocol] = {**network['summary'][protocol],
            'randomRangeMedianTtfbMs': median([s['time_starttransfer'] * 1000 for s in random]),
            'randomRangeMedianTotalMs': median([s['time_total'] * 1000 for s in random])}
    continuous = folder / 'continuous-30s.json'
    if continuous.exists():
        report['continuous30Seconds'] = [{key: s.get(key) for key in (
            'requestedProtocol', 'http_version', 'http_code', 'bodyBytes', 'observedMbps',
            'steadySeconds5Through24MedianMbps', 'largestBodyGapSeconds', 'intentionalTimedCancellation')}
            for s in json.loads(continuous.read_text())['samples']]
    for filename in ('browser.json', 'browser-h2-baseline.json', 'browser-h3-diagnostic.json', 'browser-h2-diagnostic.json'):
        path = folder / filename
        if not path.exists():
            continue
        browser = json.loads(path.read_text())
        for sample in browser['samples']:
            normalize_media(sample)
        path.write_text(json.dumps(browser, ensure_ascii=False, indent=2) + '\n')
        if filename != 'browser.json':
            report['diagnostics'][filename] = [{
                'mode': s['mode'], 'protocol': s['requestedProtocol'], 'scenario': s['scenario'], 'result': s['result'],
                'firstFrameMs': s.get('firstFrameMs'), 'initialPlayback': s.get('initialPlayback'), 'network': s.get('network'),
                'error': s.get('error'), 'seekInProgress': s.get('seekInProgress')}
                for s in browser['samples'] if s['scenario'] != 'negotiation']
            continue
        for mode in ('deployed', 'native-control'):
            report['media'][mode] = {}
            for protocol in ('h2', 'h3-auto'):
                play = [s for s in browser['samples'] if s['mode'] == mode and s['requestedProtocol'] == protocol and s['scenario'] == 'play-seek']
                weak = [s for s in browser['samples'] if s['mode'] == mode and s['requestedProtocol'] == protocol and s['scenario'] == 'offline-recovery']
                verified_weak = [s for s in weak if s.get('interruption', {}).get('networkInterruptionObserved')]
                seeks = [seek['decodedFrameMs'] for s in play for seek in s.get('seeks', [])]
                report['media'][mode][protocol] = {
                    'playSamples': len(play), 'successfulPlayAndSeekSamples': sum(s['result'] == 'measured' for s in play),
                    'firstFrameMedianMs': median([s['firstFrameMs'] for s in play if s.get('firstFrameMs') is not None]),
                    'requestCounts': [s['network']['requestCount'] for s in play],
                    'receivedBodyBytes': [s['network']['receivedBodyBytes'] for s in play],
                    'overlappingPayloadBytes': [s['network']['overlappingObservedPayloadBytes'] for s in play],
                    'waitingEventCounts': [s['network']['playbackWaitingEvents'] for s in play],
                    'stallDurationsMs': [s['network']['playbackStallMs'] for s in play],
                    'completedSeeks': len(seeks), 'plannedSeeks': 4 * len(play),
                    'completedSeekMedianMs': median(seeks), 'completedSeekMaxMs': max(seeks) if seeks else None,
                    'timedOutSeekSamples': sum('Seek timed out' in s.get('error', '') for s in play),
                    'weakSamples': len(weak), 'verifiedNetworkInterruptionSamples': len(verified_weak),
                    'sustainedRecoverySuccesses': sum(s.get('interruption', {}).get('recovery', {}).get('sustainedPlayback', False) for s in verified_weak) if verified_weak else None,
                    'stableRecoveryStartMs': [s.get('interruption', {}).get('recovery', {}).get('stablePlaybackStartedMs') for s in weak],
                    'weakWaitingEvents': [s['network']['playbackWaitingEvents'] for s in weak],
                    'weakStallMs': [s['network']['playbackStallMs'] for s in weak],
                    'observedProtocols': sorted({p for s in play + weak for p in s['network']['protocols']})}
    (folder / 'summary.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')


if __name__ == '__main__':
    main()
