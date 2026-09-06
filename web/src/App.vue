<script setup lang="ts">
import { Fa6Bolt, Fa6Brain, Fa6Robot, Fa6ScrewdriverWrench } from 'vue-icons-plus/fa6';
import { ref, onMounted, nextTick, watch } from 'vue';
import type { ChatMessage, FileNode, SessionRecord, ApprovalDecision, ApprovalMode } from './types';
import {
  fetchSessions,
  createSession,
  deleteSession,
  fetchMessages,
  fetchWorkspaceTree,
} from './api';
import { useWebSocket } from './useWebSocket';
import HeaderBar from './components/HeaderBar.vue';
import Sidebar from './components/Sidebar.vue';
import MessageItem from './components/MessageItem.vue';
import InputBar from './components/InputBar.vue';
import ApprovalModal from './components/ApprovalModal.vue';
import SettingsModal from './components/SettingsModal.vue';
import FileDrawer from './components/FileDrawer.vue';

const sessions = ref<SessionRecord[]>([]);
const currentSessionId = ref<string>('');
const workspace = ref<string>('/data/code/try/rust/oma');
const messages = ref<ChatMessage[]>([]);
const fileTree = ref<FileNode | null>(null);

const showFilesDrawer = ref(false);
const showSettingsModal = ref(false);
const chatViewportRef = ref<HTMLDivElement | null>(null);

const {
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
} = useWebSocket();

// 自动滚屏到底部
function scrollToBottom() {
  nextTick(() => {
    if (chatViewportRef.value) {
      chatViewportRef.value.scrollTop = chatViewportRef.value.scrollHeight;
    }
  });
}

watch([liveText, liveThinking, () => messages.value.length], () => {
  scrollToBottom();
});

onMounted(async () => {
  await loadSessions();
});

async function loadSessions() {
  try {
    const list = await fetchSessions(workspace.value);
    sessions.value = list;
    if (list.length > 0 && !currentSessionId.value) {
      selectSession(list[0].session_id);
    } else if (list.length === 0) {
      handleCreateSession();
    }
  } catch (e) {
    console.error('Failed to load sessions:', e);
  }
}

async function selectSession(sessionId: string) {
  currentSessionId.value = sessionId;
  try {
    messages.value = await fetchMessages(sessionId);
    scrollToBottom();
  } catch (e) {
    console.error('Failed to load messages:', e);
  }
  connect(sessionId, workspace.value);
}

async function handleCreateSession() {
  try {
    const res = await createSession({
      workspace: workspace.value,
      title: `会话 #${sessions.value.length + 1}`,
    });
    sessions.value.unshift(res.session);
    selectSession(res.session_id);
  } catch (e) {
    alert(`创建会话失败: ${e}`);
  }
}

async function handleDeleteSession(id: string) {
  if (!confirm('确定要彻底删除该会话及其所有消息历史吗？')) return;
  try {
    await deleteSession(id);
    sessions.value = sessions.value.filter((s) => s.session_id !== id);
    if (currentSessionId.value === id) {
      currentSessionId.value = '';
      if (sessions.value.length > 0) {
        selectSession(sessions.value[0].session_id);
      } else {
        messages.value = [];
      }
    }
  } catch (e) {
    alert(`删除失败: ${e}`);
  }
}

// 轮次完成后拉取最新落库历史
onTurnFinished(async () => {
  if (currentSessionId.value) {
    try {
      messages.value = await fetchMessages(currentSessionId.value);
      scrollToBottom();
    } catch (e) {
      console.error(e);
    }
  }
});

function handleSend(content: string) {
  sendCommand({
    type: 'user_input',
    data: { content },
  });
}

function handleCancel() {
  sendCancel();
}

function handleChangeModel(model: string) {
  sendCommand({
    type: 'set_model',
    data: { model },
  });
}

function handleChangeAgent(agent: string) {
  sendCommand({
    type: 'set_agent',
    data: { agent },
  });
}

function handleChangeApprovalMode(mode: ApprovalMode) {
  sendCommand({
    type: 'set_approval_mode',
    data: { mode },
  });
}

function handleForkMessage(messageId: string) {
  const newPrompt = prompt('请输入在此分叉节点下执行的新提示词：');
  if (newPrompt) {
    sendCommand({
      type: 'fork_and_run',
      data: {
        parent_message_id: messageId,
        new_content: newPrompt,
      },
    });
  }
}

function handleApprovalDecision(decision: ApprovalDecision) {
  if (pendingApproval.value) {
    sendApproval(pendingApproval.value.request_id, decision);
  }
}

async function openFilesDrawer() {
  showFilesDrawer.value = true;
  try {
    fileTree.value = await fetchWorkspaceTree(workspace.value);
  } catch (e) {
    console.error('Failed to load workspace tree:', e);
  }
}

function updateWorkspace(newWs: string) {
  workspace.value = newWs;
  loadSessions();
}
</script>

<template>
  <div class="app-layout">
    <!-- 左侧会话导航栏 -->
    <Sidebar
      :sessions="sessions"
      :current-session-id="currentSessionId"
      :workspace="workspace"
      @select-session="selectSession"
      @create-session="handleCreateSession"
      @delete-session="handleDeleteSession"
    />

    <!-- 右侧主交互区 -->
    <main class="main-content">
      <!-- 顶部状态栏 -->
      <HeaderBar
        :status="status"
        :ready="ready"
        :workspace="workspace"
        @open-files="openFilesDrawer"
        @open-settings="showSettingsModal = true"
      />

      <!-- 消息流动滚动视口 -->
      <div ref="chatViewportRef" class="chat-viewport">
        <div class="chat-inner">
          <!-- 静态持久化历史消息 -->
          <MessageItem
            v-for="msg in messages"
            :key="msg.id"
            :message="msg"
            @fork-message="handleForkMessage"
          />
          <!-- 当前轮次正在流式生成的临时呈现卡片 -->
          <div v-if="isBusy" class="message-row assistant">
            <div class="avatar assistant"><Fa6Robot /></div>
            <div class="message-column">
              <div class="message-card assistant live">
                <div class="message-header">
                  <span class="role-tag assistant">ASSISTANT</span>
                  <span class="badge badge-live"><span class="dot"></span>正在推理与执行中...</span>
                </div>

                <!-- 实时思考流 -->
                <div v-if="liveThinking" class="thinking-box">
                  <div class="thinking-header open">
                    <span><Fa6Brain style="vertical-align: -2px;" /> 实时思维链推导中...</span>
                  </div>
                  <div class="thinking-content">
                    {{ liveThinking }}
                  </div>
                </div>

                <!-- 实时文本流 -->
                <div v-if="liveText" class="message-body">
                  {{ liveText }}
                </div>

                <!-- 正在调用的工具 -->
                <div v-for="tc in activeToolCalls" :key="tc.call_id" class="tool-call-card">
                  <div class="tool-call-header">
                    <span class="tool-name-badge"><Fa6ScrewdriverWrench style="vertical-align: -2px;" /> {{ tc.name }}</span>
                    <span :class="['tool-status-badge', tc.output !== undefined ? (tc.is_error ? 'error' : 'success') : 'running']">
                      {{ tc.output !== undefined ? (tc.is_error ? '执行报错' : '执行完成') : '正在执行...' }}
                    </span>
                  </div>
                  <div class="tool-body">
                    <div class="label">// 输入参数:</div>
                    <pre>{{ JSON.stringify(tc.input, null, 2) }}</pre>
                    <div v-if="tc.output !== undefined" class="section">
                      <div class="label">// 返回结果:</div>
                      <pre>{{ tc.output }}</pre>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <div v-if="messages.length === 0 && !isBusy" class="empty-state">
            <div class="empty-icon"><Fa6Bolt /></div>
            <div class="empty-title">准备就绪，欢迎使用 Oma 协同工作台</div>
            <div class="empty-desc">输入任务需求，Oma 将自动阅读文件、执行编辑并运行测试验证代码。</div>
          </div>
        </div>
      </div>

      <!-- 底部输入框与控制面板 -->
      <InputBar
        :ready="ready"
        :is-busy="isBusy"
        @send="handleSend"
        @cancel="handleCancel"
        @change-model="handleChangeModel"
        @change-agent="handleChangeAgent"
        @change-approval-mode="handleChangeApprovalMode"
      />
    </main>

    <!-- 权限审批弹窗 (先到先得) -->
    <ApprovalModal
      v-if="pendingApproval"
      :approval="pendingApproval"
      :remaining-seconds="approvalTimerSeconds"
      @decision="handleApprovalDecision"
    />

    <!-- 系统设置弹窗 -->
    <SettingsModal
      v-if="showSettingsModal"
      :workspace="workspace"
      @close="showSettingsModal = false"
      @update-workspace="updateWorkspace"
    />

    <!-- 工作区文件树抽屉 -->
    <FileDrawer
      v-if="showFilesDrawer"
      :tree="fileTree"
      :workspace="workspace"
      @close="showFilesDrawer = false"
    />
  </div>
</template>
