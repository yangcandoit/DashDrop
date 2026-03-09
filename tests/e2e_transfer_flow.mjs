import assert from 'node:assert/strict';

function clone(obj) {
  return JSON.parse(JSON.stringify(obj));
}

function applyIncoming(state, payload) {
  state.incomingQueue = state.incomingQueue.filter((i) => i.transfer_id !== payload.transfer_id);
  state.incomingQueue.push(clone(payload));
}

function applyAccepted(state, payload, snapshotTransfer) {
  state.incomingQueue = state.incomingQueue.filter((i) => i.transfer_id !== payload.transfer_id);
  if (!state.activeTransfers[payload.transfer_id] && snapshotTransfer) {
    state.activeTransfers[payload.transfer_id] = clone(snapshotTransfer);
  }
  if (state.activeTransfers[payload.transfer_id]) {
    state.activeTransfers[payload.transfer_id].status = 'Transferring';
    state.activeTransfers[payload.transfer_id].revision = payload.revision;
  }
}

function applyTerminal(state, payload, status) {
  state.incomingQueue = state.incomingQueue.filter((i) => i.transfer_id !== payload.transfer_id);
  if (state.activeTransfers[payload.transfer_id]) {
    state.activeTransfers[payload.transfer_id].status = status;
    state.activeTransfers[payload.transfer_id].revision = payload.revision;
  }
  state.history.unshift({
    id: payload.transfer_id,
    status,
    reason_code: payload.reason_code ?? null,
    ended_at_unix: 1,
  });
}

function testIncomingAcceptHistoryLoop() {
  const state = { incomingQueue: [], activeTransfers: {}, history: [] };

  const incoming = {
    transfer_id: 't-1',
    sender_name: 'Peer A',
    sender_fp: 'fp-a',
    trusted: false,
    items: [{ file_id: 1, name: 'a.txt', rel_path: 'a.txt', size: 5 }],
    total_size: 5,
    revision: 0,
  };

  applyIncoming(state, incoming);
  assert.equal(state.incomingQueue.length, 1);

  applyAccepted(
    state,
    { transfer_id: 't-1', revision: 1 },
    {
      id: 't-1',
      direction: 'Receive',
      peer_fingerprint: 'fp-a',
      peer_name: 'Peer A',
      items: incoming.items,
      status: 'PendingAccept',
      bytes_transferred: 0,
      total_bytes: 5,
      revision: 0,
    },
  );

  assert.equal(state.incomingQueue.length, 0);
  assert.equal(state.activeTransfers['t-1'].status, 'Transferring');

  applyTerminal(state, { transfer_id: 't-1', revision: 2 }, 'Completed');
  assert.equal(state.history[0].id, 't-1');
  assert.equal(state.history[0].status, 'Completed');
}

function testIncomingRejectHistoryLoop() {
  const state = { incomingQueue: [], activeTransfers: {}, history: [] };

  applyIncoming(state, {
    transfer_id: 't-2',
    sender_name: 'Peer B',
    sender_fp: 'fp-b',
    trusted: true,
    items: [],
    total_size: 0,
    revision: 0,
  });

  state.activeTransfers['t-2'] = {
    id: 't-2',
    direction: 'Receive',
    peer_fingerprint: 'fp-b',
    peer_name: 'Peer B',
    items: [],
    status: 'PendingAccept',
    bytes_transferred: 0,
    total_bytes: 0,
    revision: 0,
  };

  applyTerminal(
    state,
    { transfer_id: 't-2', reason_code: 'E_REJECTED_BY_USER', revision: 1 },
    'Rejected',
  );

  assert.equal(state.incomingQueue.length, 0);
  assert.equal(state.activeTransfers['t-2'].status, 'Rejected');
  assert.equal(state.history[0].reason_code, 'E_REJECTED_BY_USER');
}

function testPartialAndCancelHistoryLoop() {
  const state = { incomingQueue: [], activeTransfers: {}, history: [] };

  state.activeTransfers['t-3'] = {
    id: 't-3',
    direction: 'Send',
    peer_fingerprint: 'fp-c',
    peer_name: 'Peer C',
    items: [{ file_id: 1, name: 'x.bin', rel_path: 'x.bin', size: 10 }],
    status: 'Transferring',
    bytes_transferred: 5,
    total_bytes: 10,
    revision: 3,
  };

  applyTerminal(state, { transfer_id: 't-3', revision: 4 }, 'PartialCompleted');
  assert.equal(state.activeTransfers['t-3'].status, 'PartialCompleted');
  assert.equal(state.history[0].status, 'PartialCompleted');

  state.activeTransfers['t-4'] = {
    id: 't-4',
    direction: 'Receive',
    peer_fingerprint: 'fp-d',
    peer_name: 'Peer D',
    items: [],
    status: 'Transferring',
    bytes_transferred: 1,
    total_bytes: 2,
    revision: 7,
  };

  applyTerminal(
    state,
    { transfer_id: 't-4', reason_code: 'E_CANCELLED_BY_SENDER', revision: 8 },
    'CancelledBySender',
  );
  assert.equal(state.activeTransfers['t-4'].status, 'CancelledBySender');
  assert.equal(state.history[0].reason_code, 'E_CANCELLED_BY_SENDER');
}

testIncomingAcceptHistoryLoop();
testIncomingRejectHistoryLoop();
testPartialAndCancelHistoryLoop();
console.log('frontend e2e transfer-flow: ok');
