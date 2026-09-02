import { ref } from 'vue';
import type {
  AgentCommand,
  ApprovalDecision,
  ClientMessage,
  PermissionRequestedData,
  Ready,
  ServerMessage,
  ToolCallStartedData,
} from './types';
import { getToken } from './api';

export type ConnectionStatus = 'disconnected' | 'connecting' | 'connected';

export interface ActiveToolCallState extends ToolCallStartedData {
  output?: string;
  is_error?: boolean;
}

export function useWebSocket() {
  const status = ref<ConnectionStatus>('disconnected');
  const ready = ref<Ready | null>(null);
  const isBusy = ref(false);

  // 活跃轮次实时数据
  const liveThinking = ref('');
  const liveText = ref('');
  const activeToolCalls = ref<ActiveToolCallState[]>([]);
  const pendingApproval = ref<PermissionRequestedData | null>(null);
  const approvalTimerSeconds = ref(120);

  let ws: WebSocket | null = null;
  let timerInterval: number | null = null;

  // 注册的事件监听回调
  const onTurnFinishedCallbacks: Array<() => void> = [];

  function onTurnFinished(cb: () => void) {
    onTurnFinishedCallbacks.push(cb);
  }

  function connect(sessionId: string, workspace: string) {
    if (ws) {
      ws.close();
      ws = null;
    }

    status.value = 'connecting';
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const token = getToken();
    const wsUrl = `${protocol}//${window.location.host}/ws?token=${encodeURIComponent(token)}`;

    ws = new WebSocket(wsUrl);

    ws.onopen = () => {
      status.value = 'connected';
      // 发送首包 Connect
      const connectMsg: ClientMessage = {
        kind: 'connect',
        client_id: `web_${Math.random().toString(36).substring(2, 9)}`,
        workspace,
        session_id: sessionId,
        client_type: 'web',
        client_name: 'Chrome-Web',
        version: '0.1.0',
      };
      ws?.send(JSON.stringify(connectMsg));
    };

    ws.onmessage = (event) => {
      try {
        const msg: ServerMessage = JSON.parse(event.data);
        handleServerMessage(msg);
      } catch (err) {
        console.error('Failed to parse WS message:', err);
      }
    };

    ws.onclose = () => {
      status.value = 'disconnected';
    };

    ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      status.value = 'disconnected';
    };
  }

  function handleServerMessage(msg: ServerMessage) {
    if (msg.kind === 'ready') {
      ready.value = msg.ready;
    } else if (msg.kind === 'event') {
      const ev = msg.event;
      switch (ev.type) {
        case 'turn_started':
          isBusy.value = true;
          liveThinking.value = '';
          liveText.value = '';
          activeToolCalls.value = [];
          break;

        case 'thinking_delta':
          liveThinking.value += ev.data.delta;
          break;

        case 'text_delta':
          liveText.value += ev.data.delta;
          break;

        case 'tool_call_started':
          activeToolCalls.value.push({ ...ev.data });
          break;

        case 'tool_call_finished': {
          const tc = activeToolCalls.value.find((t) => t.call_id === ev.data.call_id);
          if (tc) {
            tc.output = ev.data.output;
            tc.is_error = ev.data.is_error;
          }
          break;
        }

        case 'permission_requested':
          pendingApproval.value = ev.data;
          startApprovalTimer();
          break;

        case 'permission_resolved':
          pendingApproval.value = null;
          stopApprovalTimer();
          break;

        case 'active_turn_catch_up':
          isBusy.value = true;
          liveThinking.value = ev.data.accumulated_thinking;
          liveText.value = ev.data.accumulated_text;
          if (ev.data.active_tool_call) {
            activeToolCalls.value = [ev.data.active_tool_call];
          }
          if (ev.data.pending_approval) {
            pendingApproval.value = ev.data.pending_approval;
            startApprovalTimer();
          }
          break;

        case 'turn_finished':
          isBusy.value = false;
          onTurnFinishedCallbacks.forEach((cb) => cb());
          break;

        case 'model_changed':
          if (ready.value) ready.value.active_model = ev.data.active_model;
          break;

        case 'agent_changed':
          if (ready.value) ready.value.active_agent = ev.data.active_agent;
          break;

        case 'approval_mode_changed':
          if (ready.value) ready.value.approval_mode = ev.data.mode;
          break;

        case 'error':
          console.error('Agent error:', ev.data.message);
          break;
      }
    }
  }

  function startApprovalTimer() {
    stopApprovalTimer();
    approvalTimerSeconds.value = 120;
    timerInterval = window.setInterval(() => {
      if (approvalTimerSeconds.value > 0) {
        approvalTimerSeconds.value--;
      } else {
        stopApprovalTimer();
      }
    }, 1000);
  }

  function stopApprovalTimer() {
    if (timerInterval !== null) {
      clearInterval(timerInterval);
      timerInterval = null;
    }
  }

  function sendCommand(command: AgentCommand) {
    if (ws && status.value === 'connected') {
      const msg: ClientMessage = { kind: 'command', command };
      ws.send(JSON.stringify(msg));
    }
  }

  function sendApproval(requestId: string, decision: ApprovalDecision) {
    if (ws && status.value === 'connected') {
      const msg: ClientMessage = {
        kind: 'approval',
        response: { request_id: requestId, decision },
      };
      ws.send(JSON.stringify(msg));
      pendingApproval.value = null;
      stopApprovalTimer();
    }
  }

  function sendCancel() {
    if (ws && status.value === 'connected') {
      const msg: ClientMessage = { kind: 'cancel' };
      ws.send(JSON.stringify(msg));
      isBusy.value = false;
    }
  }

  return {
    status,
    ready,
    isBusy,
    liveThinking,
    liveText,
    activeToolCalls,
    pendingApproval,
    approvalTimerSeconds,
    connect,
    sendCommand,
    sendApproval,
    sendCancel,
    onTurnFinished,
  };
}
