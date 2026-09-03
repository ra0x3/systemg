FROM sysg-usecase-base

COPY tests/docker/usecase/restart-new-binary-cron-only-change/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-new-binary-cron-only-change/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
