package stdin

import (
	"context"
	"os"

	"go.opentelemetry.io/collector/component"
	"go.opentelemetry.io/collector/config"
	"go.opentelemetry.io/collector/consumer"
	"go.opentelemetry.io/collector/receiver/receiverhelper"
)

func NewFactory() component.ReceiverFactory {
	const typeID = "pg_otel_logs"

	return receiverhelper.NewFactory(
		typeID,
		func() config.Receiver {
			settings := config.NewReceiverSettings(config.NewID(typeID))
			return &settings
		},
		receiverhelper.WithLogs(func(
			_ context.Context, settings component.ReceiverCreateSettings,
			_ config.Receiver, next consumer.Logs,
		) (component.LogsReceiver, error) {
			return &receiver{
				source: os.Stdin, sink: next,
				ReceiverCreateSettings: settings,
			}, nil
		}),
	)
}
