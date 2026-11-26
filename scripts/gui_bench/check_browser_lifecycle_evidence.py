"""Fail closed on native acceptance evidence, not just a GUI wrapper exit code."""
import argparse
import json
from pathlib import Path

COMMON = ['BACKGROUND_OPEN_EVIDENCE', 'BACKGROUND_OPERATION_EVIDENCE',
          'BACKGROUND_CREATION_EVIDENCE', 'BACKGROUND_ISOLATION_EVIDENCE',
          'BROWSER_LIFECYCLE_EVIDENCE']
FULL = ['BROWSER_SESSION_REQUEST_EVIDENCE', 'SUBFRAME_NAVIGATION_EVIDENCE',
        'FIT_VIEWPORT_EVIDENCE', 'COMPONENT_PICKER_EVIDENCE',
        'PRODUCTION_NATIVE_EVIDENCE']


def verify(log, mode, min_idle=0):
    if 'Native observation probe failed:' in log:
        raise ValueError('native probe reported a failure')
    records = {}
    for line in log.splitlines():
        name, _, payload = line.partition(' ')
        if name in COMMON + FULL + ['BACKGROUND_SOAK_EVIDENCE']:
            value = json.loads(payload)
            if not isinstance(value, dict):
                raise ValueError(f'{name} payload must be an object')
            if name == 'SUBFRAME_NAVIGATION_EVIDENCE':
                expected = {'cross_origin': 'passed', 'same_origin': 'passed',
                            'same_document': 'passed', 'guest_caller': 'denied',
                            'loaded_owner_receipt': 'preserved',
                            'top_level_address': 'preserved'}
                if any(value.get(key) != expected_value for key, expected_value in expected.items()):
                    raise ValueError(f'{name} did not pass its exact contract')
            elif value.get('passed') is not True:
                raise ValueError(f'{name} did not pass')
            records.setdefault(name, []).append(value)
    required = COMMON + (FULL if mode == 'full' else [])
    if min_idle:
        required += ['BACKGROUND_SOAK_EVIDENCE']
    for name in required:
        if not records.get(name):
            raise ValueError(f'missing {name}')
        if name != 'BACKGROUND_OPERATION_EVIDENCE' and len(records[name]) != 1:
            raise ValueError(f'duplicate {name}')
    operations = records['BACKGROUND_OPERATION_EVIDENCE']
    if (len(operations) != 2 or {v.get('case') for v in operations} != {'switch', 'collapsed'}
            or any(v.get('dom') is not True or v.get('B_unchanged') is not True for v in operations)):
        raise ValueError('missing or invalid switch/collapsed native operation acceptance')
    if min_idle and not any(v.get('idle_seconds', 0) >= min_idle and
                            v.get('UI_renewal') is False
                            for v in records['BACKGROUND_SOAK_EVIDENCE']):
        raise ValueError('insufficient real idle duration or UI renewal was used')
    return {'passed': True, 'mode': mode, 'min_idle_seconds': min_idle,
            'evidence': records, 'model_quality_claim': False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--mode', choices=['full', 'background'], required=True)
    parser.add_argument('--min-idle', type=int, default=0)
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    try:
        receipt = verify(args.log.read_text(), args.mode, args.min_idle)
    except (ValueError, KeyError) as error:
        raise SystemExit(f'Native browser acceptance rejected: {error}') from error
    args.receipt.write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'passed': True, 'mode': args.mode, 'min_idle': args.min_idle}))


if __name__ == '__main__':
    main()
