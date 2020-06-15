ejabberd:
	ejabberdctl \
	   --config "${PWD}/ejabberd/mqtt.yml" \
	   --logs "${PWD}/ejabberd/logs" \
	   --spool "${PWD}/ejabberd/spool" \
	   foreground
.PHONY: ejabberd
