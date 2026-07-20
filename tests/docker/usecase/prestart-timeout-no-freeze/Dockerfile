FROM sysg-usecase-base

COPY tests/docker/usecase/prestart-timeout-no-freeze/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/prestart-timeout-no-freeze/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
