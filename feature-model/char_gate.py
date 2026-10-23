"""字级判别侧车服务（D.2）：微调 ELECTRA CTC 头的句级门控分数。

独立进程（.venv_char：python3.12 + torch），与 server.py（py3.15 venv）解耦；
server.py 通过 HTTP 调用（env L3_CHAR_GATE_URL 启用，未设则完全旁路）。

协议：
  GET  /health → {"status":"ok","model":...}
  POST /gate   → {"texts":[str,...]} → {"scores":[float,...]}
  分数 = 句内 top-3 字「被替换」概率均值（与 eval_char_pla.py 同口径）

启动：
  ./.venv_char/bin/python char_gate.py [--model models/char_ctc_zh] [--port 4811]
"""
import argparse
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import torch
from transformers import AutoModelForPreTraining, AutoTokenizer

HERE = os.path.dirname(os.path.abspath(__file__))
MAX_LEN = 128
TOPK = 3

_model = None
_tok = None
_dev = 'cpu'


def score_texts(texts, detail=False):
    out = []
    with torch.no_grad():
        for i in range(0, len(texts), 32):
            chunk = texts[i:i + 32]
            enc = _tok(chunk, padding=True, truncation=True,
                       max_length=MAX_LEN, return_tensors='pt').to(_dev)
            logits = _model(**enc).logits
            probs = torch.sigmoid(logits.float())
            mask = enc['attention_mask'].bool()
            for b in range(probs.shape[0]):
                p = probs[b][mask[b]][1:-1]
                k = min(TOPK, p.numel())
                t3 = round(float(torch.topk(p, k).values.mean()), 4)
                if detail:
                    out.append({'score': t3, 'chars': [round(float(x), 4) for x in p]})
                else:
                    out.append(t3)
    return out


class H(BaseHTTPRequestHandler):
    def _send(self, code, payload):
        body = json.dumps(payload, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == '/health':
            self._send(200, {'status': 'ok', 'model': ARGS.model, 'device': _dev})
        else:
            self._send(404, {'error': 'not found'})

    def do_POST(self):
        if self.path != '/gate':
            self._send(404, {'error': 'not found'})
            return
        try:
            n = int(self.headers.get('Content-Length', 0))
            body = json.loads(self.rfile.read(n) or b'{}')
            texts = [str(t) for t in body.get('texts', [])][:256]
            detail = bool(body.get('detail', False))
            self._send(200, {'scores': score_texts(texts, detail)})
        except Exception as e:  # noqa: BLE001
            self._send(500, {'error': str(e)})

    def log_message(self, *a):
        pass


def main():
    global _model, _tok, _dev
    ap = argparse.ArgumentParser()
    ap.add_argument('--model', default=os.path.join(HERE, 'models', 'char_ctc_zh_v2'))
    ap.add_argument('--port', type=int, default=int(os.environ.get('CHAR_GATE_PORT', '4811')))
    args = ap.parse_args()
    global ARGS
    ARGS = args
    _tok = AutoTokenizer.from_pretrained(args.model)
    _model = AutoModelForPreTraining.from_pretrained(args.model)
    _model.eval()
    _dev = os.environ.get('CHAR_GATE_DEVICE') or (
        'mps' if torch.backends.mps.is_available() else 'cpu')
    _model.to(_dev)
    print(f'char-gate on :{args.port} model={args.model} device={_dev}', flush=True)
    ThreadingHTTPServer(('127.0.0.1', args.port), H).serve_forever()


if __name__ == '__main__':
    main()
