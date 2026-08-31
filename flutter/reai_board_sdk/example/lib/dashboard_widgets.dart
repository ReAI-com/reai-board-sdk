import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:reai_board_sdk/reai_board_sdk.dart';

Iterable<BoardPhysicalKey> _keysForGroup(BoardKeyGroup group) =>
    switch (group) {
      BoardKeyGroup.knob => const [
        BoardPhysicalKey.knobLeft,
        BoardPhysicalKey.knobPress,
        BoardPhysicalKey.knobRight,
      ],
      _ => BoardPhysicalKey.values.where((key) => key.group == group),
    };

class AcceptanceDashboard extends StatelessWidget {
  const AcceptanceDashboard({
    required this.connected,
    required this.device,
    required this.info,
    required this.maxGattPayload,
    required this.lastEvent,
    required this.busy,
    required this.scanning,
    required this.devices,
    required this.keyConfig,
    required this.pressedKeys,
    required this.eventCounts,
    required this.selectedMode,
    required this.selectedKeyIndex,
    required this.dirtyKeys,
    required this.audioRunning,
    required this.audioFrameCount,
    required this.audioGapCount,
    required this.logs,
    required this.onScan,
    required this.onConnect,
    required this.onDisconnect,
    required this.onRefreshKeyConfig,
    required this.onSelectKey,
    required this.onBindingChanged,
    required this.onWriteKeyConfig,
    required this.onRestoreDefaults,
    required this.onToggleAudio,
    required this.onClearLogs,
    super.key,
  });

  final bool connected;
  final BleDeviceInfo? device;
  final DeviceInfo? info;
  final int maxGattPayload;
  final String lastEvent;
  final bool busy;
  final bool scanning;
  final List<BleDeviceInfo> devices;
  final KeyConfig? keyConfig;
  final Set<int> pressedKeys;
  final Map<int, int> eventCounts;
  final WorkMode? selectedMode;
  final int selectedKeyIndex;
  final Set<int> dirtyKeys;
  final bool audioRunning;
  final int audioFrameCount;
  final int audioGapCount;
  final List<String> logs;
  final VoidCallback onScan;
  final ValueChanged<BleDeviceInfo> onConnect;
  final VoidCallback onDisconnect;
  final VoidCallback onRefreshKeyConfig;
  final ValueChanged<int> onSelectKey;
  final void Function(int index, KeyBinding binding) onBindingChanged;
  final VoidCallback onWriteKeyConfig;
  final VoidCallback onRestoreDefaults;
  final VoidCallback onToggleAudio;
  final VoidCallback onClearLogs;

  @override
  Widget build(BuildContext context) => ListView(
    key: const PageStorageKey('sdk-acceptance-scroll'),
    padding: const EdgeInsets.fromLTRB(12, 4, 12, 28),
    children: [
      DeviceStatusCard(
        device: device,
        info: info,
        maxGattPayload: maxGattPayload,
        lastEvent: lastEvent,
        busy: busy,
        onDisconnect: connected ? onDisconnect : null,
      ),
      const SizedBox(height: 12),
      _SectionCard(
        title: '发现设备',
        subtitle: scanning ? '扫描中，发现一台就立即加入下方列表' : '仅显示 REAI_VB_ 蓝牙设备',
        trailing: scanning
            ? const SizedBox.square(
                dimension: 18,
                child: CircularProgressIndicator(strokeWidth: 2),
              )
            : null,
        child: Column(
          children: [
            SizedBox(
              width: double.infinity,
              child: FilledButton.icon(
                onPressed: busy ? null : onScan,
                icon: const Icon(Icons.radar),
                label: Text(
                  scanning
                      ? '扫描中…已发现 ${devices.length} 台'
                      : (busy ? '处理中…' : '扫描 REAI_VB_ 设备'),
                ),
              ),
            ),
            if (devices.isEmpty)
              const _EmptyHint(message: '尚未发现设备')
            else
              for (final candidate in devices)
                ListTile(
                  key: ValueKey('ble-device-${candidate.id}'),
                  contentPadding: EdgeInsets.zero,
                  leading: const CircleAvatar(child: Icon(Icons.bluetooth)),
                  title: Text(candidate.name),
                  subtitle: Text('${candidate.id}\nRSSI ${candidate.rssi} dBm'),
                  isThreeLine: true,
                  trailing: device?.id == candidate.id
                      ? const Chip(label: Text('已连接'))
                      : const Icon(Icons.chevron_right),
                  onTap: busy ? null : () => onConnect(candidate),
                ),
          ],
        ),
      ),
      const SizedBox(height: 12),
      HardwareInputPanel(
        config: keyConfig,
        pressedKeys: pressedKeys,
        eventCounts: eventCounts,
        selectedMode: selectedMode,
        selectedKeyIndex: selectedKeyIndex,
        dirtyKeys: dirtyKeys,
        onRefresh: connected && !busy ? onRefreshKeyConfig : null,
        onSelectKey: onSelectKey,
        onBindingChanged: onBindingChanged,
        onWrite: connected && !busy && dirtyKeys.isNotEmpty
            ? onWriteKeyConfig
            : null,
        onRestoreDefaults: connected && !busy ? onRestoreDefaults : null,
      ),
      const SizedBox(height: 12),
      _SectionCard(
        title: '板载音频',
        subtitle: audioRunning
            ? 'lease 运行中，心跳自动续租'
            : '验证 capability、lease、连续帧和丢帧数',
        trailing: _StateDot(active: audioRunning),
        child: Column(
          children: [
            Row(
              children: [
                Expanded(
                  child: _Metric(label: 'mSBC 帧', value: '$audioFrameCount'),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: _Metric(
                    label: '累计丢帧',
                    value: '$audioGapCount',
                    warning: audioGapCount > 0,
                  ),
                ),
              ],
            ),
            const SizedBox(height: 10),
            SizedBox(
              width: double.infinity,
              child: FilledButton.tonalIcon(
                onPressed: !connected || busy ? null : onToggleAudio,
                icon: Icon(audioRunning ? Icons.stop : Icons.graphic_eq),
                label: Text(audioRunning ? '停止音频 lease' : '启动音频 lease'),
              ),
            ),
          ],
        ),
      ),
      const SizedBox(height: 12),
      EventLogPanel(logs: logs, onClear: onClearLogs),
    ],
  );
}

class DeviceStatusCard extends StatelessWidget {
  const DeviceStatusCard({
    required this.device,
    required this.info,
    required this.maxGattPayload,
    required this.lastEvent,
    required this.busy,
    required this.onDisconnect,
    super.key,
  });

  final BleDeviceInfo? device;
  final DeviceInfo? info;
  final int maxGattPayload;
  final String lastEvent;
  final bool busy;
  final VoidCallback? onDisconnect;

  @override
  Widget build(BuildContext context) {
    final connected = device != null;
    return _SectionCard(
      title: '设备状态',
      subtitle: connected ? device!.name : '未连接',
      trailing: _StateDot(active: connected),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: _Metric(
                  label: '固件',
                  value: info?.firmwareVersion ?? '--',
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: _Metric(
                  label: '电量',
                  value: info == null ? '--' : '${info!.batteryLevel}%',
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: _Metric(
                  label: 'GATT 载荷',
                  value: '$maxGattPayload B',
                  warning: connected && maxGattPayload < 63,
                ),
              ),
            ],
          ),
          const SizedBox(height: 10),
          Text('最近事件：$lastEvent', style: Theme.of(context).textTheme.bodySmall),
          if (connected) ...[
            const SizedBox(height: 10),
            SizedBox(
              width: double.infinity,
              child: OutlinedButton.icon(
                onPressed: busy ? null : onDisconnect,
                icon: const Icon(Icons.link_off),
                label: const Text('断开设备'),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class HardwareInputPanel extends StatelessWidget {
  const HardwareInputPanel({
    required this.config,
    required this.pressedKeys,
    required this.eventCounts,
    required this.selectedMode,
    required this.selectedKeyIndex,
    required this.dirtyKeys,
    required this.onRefresh,
    required this.onSelectKey,
    required this.onBindingChanged,
    required this.onWrite,
    required this.onRestoreDefaults,
    super.key,
  });

  final KeyConfig? config;
  final Set<int> pressedKeys;
  final Map<int, int> eventCounts;
  final WorkMode? selectedMode;
  final int selectedKeyIndex;
  final Set<int> dirtyKeys;
  final VoidCallback? onRefresh;
  final ValueChanged<int> onSelectKey;
  final void Function(int index, KeyBinding binding) onBindingChanged;
  final VoidCallback? onWrite;
  final VoidCallback? onRestoreDefaults;

  @override
  Widget build(BuildContext context) => _SectionCard(
    title: '硬件输入',
    subtitle: config == null ? '连接后自动读取 12 键配置' : '按实体键会实时高亮并累计次数',
    trailing: IconButton(
      tooltip: '重新读取按键配置',
      onPressed: onRefresh,
      icon: const Icon(Icons.refresh),
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            const Text('当前模式'),
            const SizedBox(width: 8),
            Chip(label: Text(selectedMode?.label ?? '--')),
            const Spacer(),
            Text(
              '${eventCounts.values.fold<int>(0, (sum, count) => sum + count)} 次输入',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ],
        ),
        for (final group in BoardKeyGroup.values) ...[
          const SizedBox(height: 12),
          Text(
            group.label,
            style: Theme.of(
              context,
            ).textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w700),
          ),
          const SizedBox(height: 7),
          LayoutBuilder(
            builder: (context, constraints) {
              final tileWidth = (constraints.maxWidth - 16) / 3;
              return Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  for (final definition in _keysForGroup(group))
                    _HardwareKeyTile(
                      key: ValueKey('hardware-key-${definition.index}'),
                      width: tileWidth,
                      definition: definition,
                      binding: config?.activeBindings[definition.index],
                      pressed: pressedKeys.contains(definition.index),
                      count: eventCounts[definition.index] ?? 0,
                      selectedMode: selectedMode,
                      selected: selectedKeyIndex == definition.index,
                      dirty: dirtyKeys.contains(definition.index),
                      onTap: () => onSelectKey(definition.index),
                    ),
                ],
              );
            },
          ),
        ],
        if (config != null) ...[
          const SizedBox(height: 16),
          _BindingEditor(
            definition: BoardPhysicalKey.values[selectedKeyIndex],
            binding: config!.activeBindings[selectedKeyIndex],
            onChanged: (binding) => onBindingChanged(selectedKeyIndex, binding),
          ),
          const SizedBox(height: 12),
          Row(
            children: [
              Expanded(
                child: OutlinedButton.icon(
                  onPressed: onRestoreDefaults,
                  icon: const Icon(Icons.restore),
                  label: const Text('恢复默认'),
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: FilledButton.icon(
                  onPressed: onWrite,
                  icon: const Icon(Icons.save_outlined),
                  label: Text('写入设备（${dirtyKeys.length} 项）'),
                ),
              ),
            ],
          ),
          const SizedBox(height: 6),
          Text(
            '写入会先重读并只合并标脏键，写后再次回读验证；未知 Class 和桌面脚本绑定会原样保留。',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ],
      ],
    ),
  );
}

class _HardwareKeyTile extends StatelessWidget {
  const _HardwareKeyTile({
    required this.width,
    required this.definition,
    required this.binding,
    required this.pressed,
    required this.count,
    required this.selectedMode,
    required this.selected,
    required this.dirty,
    required this.onTap,
    super.key,
  });

  final double width;
  final BoardPhysicalKey definition;
  final KeyBinding? binding;
  final bool pressed;
  final int count;
  final WorkMode? selectedMode;
  final bool selected;
  final bool dirty;
  final VoidCallback onTap;

  bool get _modeSelected => switch (definition) {
    BoardPhysicalKey.yolo => selectedMode == WorkMode.yolo,
    BoardPhysicalKey.plan => selectedMode == WorkMode.plan,
    BoardPhysicalKey.chat => selectedMode == WorkMode.chat,
    _ => false,
  };

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final active = pressed || _modeSelected || selected;
    return Material(
      color: Colors.transparent,
      child: InkWell(
        borderRadius: BorderRadius.circular(14),
        splashColor: Colors.transparent,
        highlightColor: Colors.transparent,
        onTap: () {
          HapticFeedback.selectionClick();
          onTap();
        },
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          width: width,
          height: 112,
          padding: const EdgeInsets.all(9),
          decoration: BoxDecoration(
            color: pressed
                ? colors.primaryContainer
                : (_modeSelected ? colors.secondaryContainer : colors.surface),
            borderRadius: BorderRadius.circular(14),
            border: Border.all(
              color: active ? colors.primary : colors.outlineVariant,
              width: active ? 2 : 1,
            ),
            boxShadow: pressed
                ? [
                    BoxShadow(
                      color: colors.primary.withValues(alpha: 0.18),
                      blurRadius: 10,
                      spreadRadius: 1,
                    ),
                  ]
                : null,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Text(
                      definition.label,
                      maxLines: 1,
                      style: const TextStyle(fontWeight: FontWeight.w800),
                    ),
                  ),
                  if (dirty)
                    Container(
                      width: 7,
                      height: 7,
                      margin: const EdgeInsets.only(right: 4),
                      decoration: BoxDecoration(
                        color: colors.error,
                        shape: BoxShape.circle,
                      ),
                    ),
                  Text(
                    'K${definition.index}',
                    style: Theme.of(context).textTheme.labelSmall,
                  ),
                ],
              ),
              const SizedBox(height: 6),
              Text(
                binding?.description ?? '等待配置',
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const Spacer(),
              Text(
                pressed ? '按下 · $count 次' : '$count 次',
                style: Theme.of(context).textTheme.labelSmall?.copyWith(
                  color: active ? colors.primary : colors.onSurfaceVariant,
                  fontWeight: active ? FontWeight.w700 : null,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _BindingEditor extends StatelessWidget {
  const _BindingEditor({
    required this.definition,
    required this.binding,
    required this.onChanged,
  });

  final BoardPhysicalKey definition;
  final KeyBinding binding;
  final ValueChanged<KeyBinding> onChanged;

  static const options = <KeyBinding>[
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F04),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F01),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F02),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F03),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F06),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F05),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F0A),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F0B),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F0C),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F07),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F08),
    KeyBinding(keyClass: BoardKeyClass.media, keyValue: 0x0F09),
    KeyBinding(keyClass: BoardKeyClass.aiVoice, keyValue: 0),
    KeyBinding(keyClass: BoardKeyClass.disabled, keyValue: 0),
  ];

  @override
  Widget build(BuildContext context) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    decoration: BoxDecoration(
      color: Theme.of(context).colorScheme.surfaceContainerLow,
      borderRadius: BorderRadius.circular(14),
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          '当前绑定 · KEY${definition.index} ${definition.name}',
          style: Theme.of(
            context,
          ).textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w800),
        ),
        const SizedBox(height: 4),
        Text(
          'Class 0x${binding.keyClass.toRadixString(16).toUpperCase().padLeft(2, '0')}  '
          'Value 0x${binding.keyValue.toRadixString(16).toUpperCase().padLeft(4, '0')}  '
          '${binding.description}',
          style: Theme.of(context).textTheme.bodySmall,
        ),
        const SizedBox(height: 10),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final option in options)
              ChoiceChip(
                label: Text(option.description),
                selected: option == binding,
                onSelected: (_) => onChanged(option),
              ),
          ],
        ),
        const SizedBox(height: 8),
        Text(
          binding.keyClass == BoardKeyClass.keyboard
              ? '当前键盘组合会保留；手机验收页不模拟桌面实体键盘捕获。'
              : '脚本触发值只读保留，手机端不执行桌面脚本。',
          style: Theme.of(context).textTheme.labelSmall,
        ),
      ],
    ),
  );
}

class EventLogPanel extends StatelessWidget {
  const EventLogPanel({required this.logs, required this.onClear, super.key});

  final List<String> logs;
  final VoidCallback onClear;

  @override
  Widget build(BuildContext context) => Card(
    clipBehavior: Clip.antiAlias,
    child: ExpansionTile(
      key: const PageStorageKey('event-log-panel'),
      initiallyExpanded: true,
      title: const Text('事件日志', style: TextStyle(fontWeight: FontWeight.w700)),
      subtitle: Text('保留最近 ${logs.length}/200 条，并同步输出到系统日志'),
      trailing: IconButton(
        tooltip: '清空日志',
        onPressed: logs.isEmpty
            ? null
            : () {
                HapticFeedback.lightImpact();
                onClear();
              },
        icon: const Icon(Icons.delete_sweep_outlined),
      ),
      children: [
        if (logs.isEmpty)
          const _EmptyHint(message: '暂无日志')
        else
          Container(
            width: double.infinity,
            constraints: const BoxConstraints(maxHeight: 320),
            color: const Color(0xFF171A22),
            child: ListView.separated(
              key: const ValueKey('event-log-list'),
              padding: const EdgeInsets.all(12),
              shrinkWrap: true,
              itemCount: logs.length,
              separatorBuilder: (_, _) => const SizedBox(height: 7),
              itemBuilder: (context, index) => SelectableText(
                logs[index],
                style: const TextStyle(
                  color: Color(0xFFD8DCE9),
                  fontFamily: 'monospace',
                  fontSize: 11,
                  height: 1.35,
                ),
              ),
            ),
          ),
      ],
    ),
  );
}

class _SectionCard extends StatelessWidget {
  const _SectionCard({
    required this.title,
    required this.subtitle,
    required this.child,
    this.trailing,
  });

  final String title;
  final String subtitle;
  final Widget child;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) => Card(
    shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(18)),
    child: Padding(
      padding: const EdgeInsets.all(14),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      title,
                      style: Theme.of(context).textTheme.titleMedium?.copyWith(
                        fontWeight: FontWeight.w800,
                      ),
                    ),
                    const SizedBox(height: 2),
                    Text(
                      subtitle,
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ],
                ),
              ),
              ?trailing,
            ],
          ),
          const SizedBox(height: 14),
          child,
        ],
      ),
    ),
  );
}

class _Metric extends StatelessWidget {
  const _Metric({
    required this.label,
    required this.value,
    this.warning = false,
  });

  final String label;
  final String value;
  final bool warning;

  @override
  Widget build(BuildContext context) => Container(
    padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 9),
    decoration: BoxDecoration(
      color: warning
          ? Theme.of(context).colorScheme.errorContainer
          : Theme.of(context).colorScheme.surfaceContainerLow,
      borderRadius: BorderRadius.circular(12),
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(label, style: Theme.of(context).textTheme.labelSmall),
        const SizedBox(height: 2),
        Text(
          value,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: Theme.of(
            context,
          ).textTheme.titleSmall?.copyWith(fontWeight: FontWeight.w800),
        ),
      ],
    ),
  );
}

class _StateDot extends StatelessWidget {
  const _StateDot({required this.active});

  final bool active;

  @override
  Widget build(BuildContext context) => Container(
    width: 12,
    height: 12,
    margin: const EdgeInsets.all(8),
    decoration: BoxDecoration(
      color: active ? const Color(0xFF22A06B) : const Color(0xFFB8BEC9),
      shape: BoxShape.circle,
      boxShadow: active
          ? const [
              BoxShadow(
                color: Color(0x5522A06B),
                blurRadius: 8,
                spreadRadius: 2,
              ),
            ]
          : null,
    ),
  );
}

class _EmptyHint extends StatelessWidget {
  const _EmptyHint({required this.message});

  final String message;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 16),
    child: Center(
      child: Text(message, style: Theme.of(context).textTheme.bodySmall),
    ),
  );
}
