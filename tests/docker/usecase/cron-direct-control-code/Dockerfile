FROM sysg-usecase-base

COPY tests/docker/usecase/cron-direct-control-code/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/cron-direct-control-code/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
